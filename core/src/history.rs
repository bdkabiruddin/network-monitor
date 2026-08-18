use std::path::{Path, PathBuf};

use chrono::Local;
use rusqlite::{params, Connection};

/// Prefer XDG_DATA_HOME (or ~/.local/share) so the app works even when
/// installed to a read-only location, and so it shares the same on-disk
/// database the Python app already wrote to.
pub fn get_data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(std::env::temp_dir);
    let target = base.join("network-monitor");
    if std::fs::create_dir_all(&target).is_ok() {
        target
    } else {
        std::env::temp_dir()
    }
}

/// Default history retention -- overridden by the user-configurable
/// setting (Settings screen), this is only the fallback used for the
/// safety-net prune that runs at DB-open time, before any setting has
/// been read.
pub const DEFAULT_RETENTION_DAYS: i64 = 30;

pub struct Reading {
    pub default_iface: Option<String>,
    pub download_rate: Option<f64>,
    pub upload_rate: Option<f64>,
    pub total_downloaded: Option<f64>,
    pub total_uploaded: Option<f64>,
    pub latency_ms: Option<f64>,
}

pub struct GraphRow {
    pub timestamp: String,
    pub download_rate: Option<f64>,
    pub upload_rate: Option<f64>,
    pub latency_ms: Option<f64>,
}

/// All public methods are defensive: sqlite I/O can fail (disk full,
/// locked file, corrupted db, permissions) and we never want that to
/// take down the update loop that drives the whole UI.
pub struct HistoryDb {
    conn: Option<Connection>,
}

fn log_error(context: &str, err: impl std::fmt::Display) {
    eprintln!("[network-monitor] {context}: {err}");
}

impl HistoryDb {
    pub fn open(path: &Path) -> Self {
        let conn = match Connection::open(path) {
            Ok(conn) => {
                // WAL mode lets the background writer and ad hoc UI-thread
                // reader connections coexist without "database is locked"
                // errors under the default rollback journal.
                let init = conn.execute_batch(
                    "PRAGMA journal_mode=WAL;
                    PRAGMA busy_timeout=2000;
                    CREATE TABLE IF NOT EXISTS readings (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        timestamp TEXT NOT NULL,
                        iface TEXT,
                        download_rate REAL,
                        upload_rate REAL,
                        total_downloaded REAL,
                        total_uploaded REAL,
                        latency_ms REAL
                    );
                    CREATE INDEX IF NOT EXISTS idx_time ON readings(timestamp);",
                );
                match init {
                    Ok(()) => Some(conn),
                    Err(e) => {
                        log_error("failed to initialize database", e);
                        None
                    }
                }
            }
            Err(e) => {
                log_error("failed to open database", e);
                None
            }
        };
        let db = Self { conn };
        db.prune(DEFAULT_RETENTION_DAYS);
        db
    }

    fn now_iso() -> String {
        Local::now().format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }

    pub fn prune(&self, keep_days: i64) {
        let Some(conn) = &self.conn else { return };
        // A `keep_days` of 0 or less (only reachable via a hand-edited
        // settings.json -- the UI slider clamps to 1-365) would put the
        // cutoff in the *future*, deleting the entire table. Clamped
        // here rather than trusted, since this runs on every save and
        // hourly regardless of what wrote the setting.
        let keep_days = keep_days.max(1);
        let cutoff = (Local::now() - chrono::Duration::days(keep_days))
            .format("%Y-%m-%dT%H:%M:%S%.6f")
            .to_string();
        if let Err(e) = conn.execute("DELETE FROM readings WHERE timestamp < ?1", params![cutoff]) {
            log_error("prune failed", e);
        }
    }

    pub fn save(&self, data: &Reading) {
        let Some(conn) = &self.conn else { return };
        let result = conn.execute(
            "INSERT INTO readings (timestamp, iface, download_rate, upload_rate, total_downloaded, total_uploaded, latency_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                Self::now_iso(),
                data.default_iface.clone().unwrap_or_default(),
                data.download_rate,
                data.upload_rate,
                data.total_downloaded,
                data.total_uploaded,
                data.latency_ms,
            ],
        );
        if let Err(e) = result {
            log_error("save failed", e);
        }
    }

    pub fn row_count(&self) -> i64 {
        let Some(conn) = &self.conn else { return 0 };
        conn.query_row("SELECT COUNT(*) FROM readings", [], |row| row.get(0)).unwrap_or(0)
    }

    pub fn clear_all(&self) {
        let Some(conn) = &self.conn else { return };
        if let Err(e) = conn.execute("DELETE FROM readings", []) {
            log_error("clear_all failed", e);
        }
    }

    /// Rows for the telemetry chart over the last `seconds`. Large
    /// ranges are bucketed in SQL (AVG per time bucket) rather than
    /// returned raw. `bucket_seconds` picks the average explicitly; left
    /// as None, the bucket size is chosen automatically: raw rows for a
    /// short range, or seconds/graph_points for a long one.
    pub fn graph_data(&self, seconds: i64, bucket_seconds: Option<i64>, graph_points: i64) -> Vec<GraphRow> {
        let Some(conn) = &self.conn else { return Vec::new() };
        let since = (Local::now() - chrono::Duration::seconds(seconds))
            .format("%Y-%m-%dT%H:%M:%S%.6f")
            .to_string();

        let query_raw = bucket_seconds.is_none() && seconds <= 3600;
        let result = if query_raw {
            conn.prepare(
                "SELECT timestamp, download_rate, upload_rate, latency_ms
                 FROM readings WHERE timestamp >= ?1 ORDER BY timestamp",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![since], |row| {
                    Ok(GraphRow {
                        timestamp: row.get(0)?,
                        download_rate: row.get(1)?,
                        upload_rate: row.get(2)?,
                        latency_ms: row.get(3)?,
                    })
                })
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            })
        } else {
            let bucket = bucket_seconds.unwrap_or_else(|| (seconds / graph_points).max(1)).max(1);
            conn.prepare(
                "SELECT
                    datetime((strftime('%s', timestamp) / ?1) * ?1, 'unixepoch') AS bucket,
                    AVG(download_rate), AVG(upload_rate), AVG(latency_ms)
                 FROM readings
                 WHERE timestamp >= ?2
                 GROUP BY bucket
                 ORDER BY bucket",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![bucket, since], |row| {
                    Ok(GraphRow {
                        timestamp: row.get(0)?,
                        download_rate: row.get(1)?,
                        upload_rate: row.get(2)?,
                        latency_ms: row.get(3)?,
                    })
                })
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            })
        };

        match result {
            Ok(rows) => rows,
            Err(e) => {
                log_error("graph_data failed", e);
                Vec::new()
            }
        }
    }

    /// Average combined download+upload rate per bin over the last
    /// `hours` — powers the 24H Traffic chart.
    pub fn interval_totals(&self, hours: i64, bins: i64) -> Vec<f64> {
        self.interval_totals_split(hours, bins).into_iter().map(|(dl, ul)| dl + ul).collect()
    }

    /// Same as [`Self::interval_totals`], but keeps download and upload
    /// separate per bin instead of summing them — for a chart that shows
    /// both series rather than one combined line.
    ///
    /// Averages the rate samples that fall in each bin (rather than
    /// summing them) so the result stays a proper bytes/sec rate
    /// regardless of poll cadence — summing raw B/s samples scales with
    /// however many samples happened to land in the bin (more of them
    /// for a coarser bin size), which reads as an impossibly large
    /// "rate" once formatted with a rate unit.
    pub fn interval_totals_split(&self, hours: i64, bins: i64) -> Vec<(f64, f64)> {
        let empty = vec![(0.0_f64, 0.0_f64); bins.max(0) as usize];
        let Some(conn) = &self.conn else { return empty };
        if bins <= 0 {
            return empty;
        }

        let since = Local::now() - chrono::Duration::hours(hours);
        let bin_seconds = (hours * 3600) as f64 / bins as f64;
        let since_str = since.format("%Y-%m-%dT%H:%M:%S%.6f").to_string();

        // Bin in SQL (bucket index + AVG per bucket) instead of pulling every
        // raw row into Rust and parsing each timestamp by hand — with a
        // history table that can hold days of 2s-cadence samples, doing that
        // parse-and-bin loop on every poll tick is the difference between a
        // handful of aggregated rows and tens of thousands of round trips.
        let result = conn
            .prepare(
                "SELECT
                    CAST((julianday(timestamp) - julianday(?1)) * 86400.0 / ?2 AS INTEGER) AS bin,
                    AVG(download_rate), AVG(upload_rate)
                 FROM readings
                 WHERE timestamp >= ?1
                 GROUP BY bin",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![since_str, bin_seconds], |row| {
                    let bin: i64 = row.get(0)?;
                    let dl: Option<f64> = row.get(1)?;
                    let ul: Option<f64> = row.get(2)?;
                    Ok((bin, dl, ul))
                })
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            });

        let rows = match result {
            Ok(rows) => rows,
            Err(e) => {
                log_error("interval_totals failed", e);
                return empty;
            }
        };

        let mut out = empty;
        for (bin, dl, ul) in rows {
            if bin >= 0 && (bin as usize) < out.len() {
                out[bin as usize] = (dl.unwrap_or(0.0), ul.unwrap_or(0.0));
            }
        }
        out
    }

    /// Average latency per bin over the last `hours` — same fixed-width
    /// binning as [`Self::interval_totals_split`], for a chart that
    /// overlays latency alongside bandwidth. `None` for a bin with no
    /// latency samples (offline, or every sample in it failed to ping).
    pub fn interval_latency_avg(&self, hours: i64, bins: i64) -> Vec<Option<f64>> {
        let out_len = bins.max(0) as usize;
        let Some(conn) = &self.conn else { return vec![None; out_len] };
        if bins <= 0 {
            return Vec::new();
        }

        let since = Local::now() - chrono::Duration::hours(hours);
        let bin_seconds = (hours * 3600) as f64 / bins as f64;
        let since_str = since.format("%Y-%m-%dT%H:%M:%S%.6f").to_string();

        // Same SQL-side binning as `interval_totals_split` — see that
        // method's comment for why this can't be a per-row Rust loop.
        let result = conn
            .prepare(
                "SELECT
                    CAST((julianday(timestamp) - julianday(?1)) * 86400.0 / ?2 AS INTEGER) AS bin,
                    AVG(latency_ms)
                 FROM readings
                 WHERE timestamp >= ?1 AND latency_ms IS NOT NULL
                 GROUP BY bin",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![since_str, bin_seconds], |row| {
                    let bin: i64 = row.get(0)?;
                    let lat: Option<f64> = row.get(1)?;
                    Ok((bin, lat))
                })
                .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            });

        let rows = match result {
            Ok(rows) => rows,
            Err(e) => {
                log_error("interval_latency_avg failed", e);
                return vec![None; out_len];
            }
        };

        let mut out = vec![None; out_len];
        for (bin, lat) in rows {
            if bin >= 0 && (bin as usize) < out.len() {
                out[bin as usize] = lat;
            }
        }
        out
    }

    /// Real bytes transferred since `since_iso` (an ISO timestamp string),
    /// for comparing against Settings' data cap. `total_downloaded`/
    /// `total_uploaded` are cumulative interface counters, not per-sample
    /// deltas, so this sums consecutive-row diffs in SQL (window function,
    /// same reasoning as `interval_totals_split`'s comment: a per-row Rust
    /// loop over a month of 2s-cadence history doesn't scale) rather than
    /// reading the raw counters directly — a reboot or NIC reconnect
    /// resets those counters mid-period, and a naive
    /// `last_total - first_total` would go negative or silently
    /// undercount across that reset. Negative diffs (a reset) are treated
    /// as 0 for that gap instead of subtracted, so a reset can only ever
    /// undercount the (small) usage right around it, never produce a
    /// nonsensical total.
    pub fn usage_since(&self, since_iso: &str) -> (f64, f64) {
        let Some(conn) = &self.conn else { return (0.0, 0.0) };
        let result = conn.query_row(
            "SELECT
                COALESCE(SUM(MAX(dl_diff, 0)), 0), COALESCE(SUM(MAX(ul_diff, 0)), 0)
             FROM (
                SELECT
                    total_downloaded - LAG(total_downloaded) OVER (ORDER BY timestamp) AS dl_diff,
                    total_uploaded - LAG(total_uploaded) OVER (ORDER BY timestamp) AS ul_diff
                FROM readings WHERE timestamp >= ?1
             )",
            params![since_iso],
            |row| Ok((row.get::<_, f64>(0)?, row.get::<_, f64>(1)?)),
        );
        match result {
            Ok(v) => v,
            Err(e) => {
                log_error("usage_since failed", e);
                (0.0, 0.0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_query_round_trip() {
        let dir = std::env::temp_dir().join(format!("netmon-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");
        let db = HistoryDb::open(&db_path);

        db.save(&Reading {
            default_iface: Some("eth0".into()),
            download_rate: Some(1234.0),
            upload_rate: Some(567.0),
            total_downloaded: Some(1_000_000.0),
            total_uploaded: Some(500_000.0),
            latency_ms: Some(12.5),
        });

        let rows = db.graph_data(3600, None, 720);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].download_rate, Some(1234.0));

        db.clear_all();
        assert_eq!(db.graph_data(3600, None, 720).len(), 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prune_clamps_a_non_positive_keep_days_to_avoid_wiping_everything() {
        let dir = std::env::temp_dir().join(format!("netmon-test-prune-clamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");
        let db = HistoryDb::open(&db_path);

        db.save(&Reading {
            default_iface: Some("eth0".into()),
            download_rate: Some(1.0),
            upload_rate: Some(1.0),
            total_downloaded: Some(1.0),
            total_uploaded: Some(1.0),
            latency_ms: Some(1.0),
        });

        // Only reachable via a hand-edited settings.json -- the UI slider
        // clamps to 1-365 -- but a cutoff of "now + N days" would delete
        // every row just saved if this weren't clamped to a minimum of 1.
        db.prune(0);
        assert_eq!(db.graph_data(3600, None, 720).len(), 1, "a non-positive keep_days must not wipe today's data");

        db.prune(-5);
        assert_eq!(db.graph_data(3600, None, 720).len(), 1, "a negative keep_days must not wipe today's data either");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn interval_binning_averages_by_bin_not_globally() {
        let dir = std::env::temp_dir().join(format!("netmon-test-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");
        let db = HistoryDb::open(&db_path);
        let conn = db.conn.as_ref().unwrap();

        // Two samples in the most-recent bin (last of 4, over the last hour),
        // one sample in an earlier bin — explicit timestamps so this doesn't
        // depend on wall-clock timing while the test runs.
        let now = Local::now();
        let insert = |offset_secs: i64, dl: f64, ul: f64, lat: f64| {
            let ts = (now - chrono::Duration::seconds(offset_secs))
                .format("%Y-%m-%dT%H:%M:%S%.6f")
                .to_string();
            conn.execute(
                "INSERT INTO readings (timestamp, iface, download_rate, upload_rate, total_downloaded, total_uploaded, latency_ms)
                 VALUES (?1, 'eth0', ?2, ?3, 0, 0, ?4)",
                params![ts, dl, ul, lat],
            )
            .unwrap();
        };
        // 1 hour = 4 bins of 15 min each. Bin 3 (most recent) gets two
        // samples that should be averaged, not summed.
        insert(60, 100.0, 10.0, 20.0);
        insert(30, 300.0, 30.0, 40.0);
        // Bin 0 (oldest, ~55 min ago) gets one sample.
        insert(55 * 60, 40.0, 4.0, 8.0);

        let totals = db.interval_totals_split(1, 4);
        assert_eq!(totals.len(), 4);
        assert_eq!(totals[0], (40.0, 4.0));
        assert_eq!(totals[3], (200.0, 20.0)); // average of 100/300 and 10/30, not their sum

        let latency = db.interval_latency_avg(1, 4);
        assert_eq!(latency[0], Some(8.0));
        assert_eq!(latency[3], Some(30.0));
        assert_eq!(latency[1], None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usage_since_sums_diffs_and_ignores_a_counter_reset() {
        let dir = std::env::temp_dir().join(format!("netmon-test-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("test.db");
        let db = HistoryDb::open(&db_path);
        let conn = db.conn.as_ref().unwrap();

        let now = Local::now();
        let insert = |offset_secs: i64, total_dl: f64, total_ul: f64| {
            let ts = (now - chrono::Duration::seconds(offset_secs))
                .format("%Y-%m-%dT%H:%M:%S%.6f")
                .to_string();
            conn.execute(
                "INSERT INTO readings (timestamp, iface, download_rate, upload_rate, total_downloaded, total_uploaded, latency_ms)
                 VALUES (?1, 'eth0', 0, 0, ?2, ?3, NULL)",
                params![ts, total_dl, total_ul],
            )
            .unwrap();
        };
        // Rising counters: +500 downloaded, +100 uploaded across two hops.
        insert(300, 1_000.0, 200.0);
        insert(200, 1_300.0, 250.0);
        insert(100, 1_500.0, 300.0);
        // A reset (reboot/NIC reconnect): counter drops back near zero.
        // The negative diff here must not subtract from the running total.
        insert(50, 50.0, 10.0);
        insert(0, 150.0, 40.0);

        let since = (now - chrono::Duration::seconds(400)).format("%Y-%m-%dT%H:%M:%S%.6f").to_string();
        let (dl, ul) = db.usage_since(&since);
        // (1300-1000) + (1500-1300) + reset-gap skipped + (150-50) = 600
        assert_eq!(dl, 600.0);
        // (250-200) + (300-250) + reset-gap skipped + (40-10) = 130
        assert_eq!(ul, 130.0);

        std::fs::remove_dir_all(&dir).ok();
    }
}

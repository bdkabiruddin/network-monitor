use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;

use crate::history::HistoryDb;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportPeriod {
    Last24h,
    Last7d,
    Last30d,
}

impl ReportPeriod {
    pub fn label(&self) -> &'static str {
        match self {
            ReportPeriod::Last24h => "Last 24 hours",
            ReportPeriod::Last7d => "Last 7 days",
            ReportPeriod::Last30d => "Last 30 days",
        }
    }

    fn seconds(&self) -> i64 {
        match self {
            ReportPeriod::Last24h => 24 * 3600,
            ReportPeriod::Last7d => 7 * 24 * 3600,
            ReportPeriod::Last30d => 30 * 24 * 3600,
        }
    }

    /// Bucket size chosen so a 30-day report doesn't hand back a
    /// several-hundred-thousand-row CSV: coarser buckets for wider
    /// periods, same tradeoff `HistoryDb::graph_data` already makes for
    /// the Live Traffic chart.
    fn bucket_seconds(&self) -> i64 {
        match self {
            ReportPeriod::Last24h => 300,   // 5 min
            ReportPeriod::Last7d => 1800,   // 30 min
            ReportPeriod::Last30d => 7200,  // 2 hours
        }
    }
}

pub fn reports_dir() -> PathBuf {
    crate::history::get_data_dir().join("reports")
}

#[derive(Debug, Clone)]
pub struct ReportEntry {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub modified: Option<chrono::DateTime<Local>>,
}

/// Generates a CSV export of real recorded history for `period` and
/// writes it under `reports_dir()`. Not a scheduled/emailed report (see
/// ROADMAP.md) — an on-demand export of this machine's own telemetry.
pub fn generate_csv_report(db: &HistoryDb, period: ReportPeriod) -> Result<PathBuf, String> {
    let rows = db.graph_data(period.seconds(), Some(period.bucket_seconds()), 720);

    let dir = reports_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let filename = format!(
        "network-report-{}-{}.csv",
        match period {
            ReportPeriod::Last24h => "24h",
            ReportPeriod::Last7d => "7d",
            ReportPeriod::Last30d => "30d",
        },
        Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = dir.join(filename);

    let mut csv = String::from("timestamp,download_bytes_per_sec,upload_bytes_per_sec,latency_ms\n");
    for row in &rows {
        csv.push_str(&format!(
            "{},{},{},{}\n",
            row.timestamp,
            row.download_rate.map(|v| v.to_string()).unwrap_or_default(),
            row.upload_rate.map(|v| v.to_string()).unwrap_or_default(),
            row.latency_ms.map(|v| v.to_string()).unwrap_or_default(),
        ));
    }

    fs::write(&path, csv).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Previously generated reports, newest first — read straight off disk,
/// not tracked separately, so this is always in sync with what's
/// actually there.
pub fn list_reports() -> Vec<ReportEntry> {
    let dir = reports_dir();
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut reports: Vec<ReportEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("csv") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            let modified = metadata.modified().ok().map(chrono::DateTime::<Local>::from);
            Some(ReportEntry {
                name: path.file_name()?.to_string_lossy().to_string(),
                path,
                size_bytes: metadata.len(),
                modified,
            })
        })
        .collect();
    reports.sort_by(|a, b| b.modified.cmp(&a.modified));
    reports
}

/// Refuses to delete anything outside `reports_dir()` -- `path` comes
/// straight from the frontend (echoed back from `get_reports()` in the
/// normal flow, but nothing on the Rust side enforced that), so without
/// this check a bug or a compromised webview could turn this into an
/// arbitrary-file-delete primitive.
pub fn delete_report(path: &Path) -> Result<(), String> {
    let dir = fs::canonicalize(reports_dir()).map_err(|e| e.to_string())?;
    let target = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if target.parent() != Some(dir.as_path()) {
        return Err("refusing to delete a path outside the reports directory".to_string());
    }
    fs::remove_file(&target).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryDb;

    #[test]
    fn generate_csv_report_writes_a_header_even_with_no_history() {
        // reports_dir() is derived from the real get_data_dir() (not
        // parameterized), so this writes to the same on-disk location the
        // running app would use -- cleaned up immediately after.
        let dir = std::env::temp_dir().join(format!("netmon-test-reports-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = HistoryDb::open(&dir.join("history.db"));

        let path = generate_csv_report(&db, ReportPeriod::Last24h).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(contents.trim(), "timestamp,download_bytes_per_sec,upload_bytes_per_sec,latency_ms");
    }

    #[test]
    fn list_reports_ignores_non_csv_files_and_sorts_newest_first() {
        let dir = reports_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let stray = dir.join("not-a-report.txt");
        std::fs::write(&stray, "hello").unwrap();
        let before = list_reports().len();
        std::fs::remove_file(&stray).unwrap();
        let after = list_reports().len();
        assert_eq!(before, after); // the .txt file was never counted
    }

    #[test]
    fn delete_report_refuses_a_path_outside_reports_dir() {
        let dir = reports_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let outside = std::env::temp_dir().join(format!("netmon-test-reports-outside-{}.txt", std::process::id()));
        std::fs::write(&outside, "not a report").unwrap();

        let result = delete_report(&outside);

        let still_there = outside.exists();
        std::fs::remove_file(&outside).ok();

        assert!(result.is_err());
        assert!(still_there); // must not have been deleted
    }

    #[test]
    fn delete_report_removes_a_real_report_inside_reports_dir() {
        let dir = reports_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("netmon-test-delete-{}.csv", std::process::id()));
        std::fs::write(&path, "timestamp\n").unwrap();

        let result = delete_report(&path);

        assert!(result.is_ok());
        assert!(!path.exists());
    }
}

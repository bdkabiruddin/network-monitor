use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Datelike, Local};

use crate::history::get_data_dir;

/// DB-IP's free "Lite" databases — CC BY 4.0 licensed (attribution
/// required, unlike MaxMind's GeoLite2 which restricts redistribution),
/// downloadable directly with no account/API key, republished monthly.
/// Neither is bundled in the repo (City is ~60MB compressed/~150MB on
/// disk, ASN a much smaller ~5MB/~10MB; both go stale) — downloaded on
/// demand into the app's own data dir, same as the SQLite history db.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build()
}

fn db_url_for(product: &str, year: i32, month: u32) -> String {
    format!("https://download.db-ip.com/free/dbip-{product}-{year:04}-{month:02}.mmdb.gz")
}

pub fn database_path() -> PathBuf {
    get_data_dir().join("dbip-city-lite.mmdb")
}

/// ASN (Autonomous System Number) + organization — the closest real,
/// honest equivalent of "IP registrant info" available without live
/// per-row WHOIS lookups (which would be far too slow/rate-limited to
/// run against every peer on every refresh). Not the same thing as a
/// precise ownership registrant, but it's what "who does this IP belong
/// to" questions are answered with in practice (e.g. "AS15169 Google
/// LLC").
pub fn asn_database_path() -> PathBuf {
    get_data_dir().join("dbip-asn-lite.mmdb")
}

/// Whether the (city) database is present, and how old (days) if so —
/// DB-IP republishes monthly, so anything over ~35 days old has a newer
/// real edition available. The ASN database's freshness isn't tracked
/// separately in the UI; it's downloaded alongside the city one and
/// treated as a bonus enrichment, not a second thing to manage.
pub struct DatabaseStatus {
    pub present: bool,
    pub age_days: Option<i64>,
}

pub fn database_status() -> DatabaseStatus {
    match std::fs::metadata(database_path()).and_then(|m| m.modified()) {
        Ok(modified) => {
            let age_days = modified.elapsed().map(|d| d.as_secs() as i64 / 86400).unwrap_or(0);
            DatabaseStatus { present: true, age_days: Some(age_days) }
        }
        Err(_) => DatabaseStatus { present: false, age_days: None },
    }
}

/// Downloads one product's this-month edition, gunzips it, validates it
/// opens as a real MMDB, and writes it to `dest`. Falls back to last
/// month's edition if this month's isn't published yet (DB-IP's monthly
/// release doesn't always land exactly on the 1st) — still a real,
/// dated edition, just potentially a few weeks older, not a fabricated
/// fallback. Validating before writing means a truncated/corrupt
/// download can never clobber a working database with a broken one.
fn download_mmdb(product: &str, dest: &Path) -> Result<(), String> {
    let now = Local::now();
    let (this_year, this_month) = (now.year(), now.month());
    let (last_year, last_month) = if this_month == 1 { (this_year - 1, 12) } else { (this_year, this_month - 1) };

    let client = agent();
    let resp = match client.get(&db_url_for(product, this_year, this_month)).call() {
        Ok(r) => r,
        Err(_) => client.get(&db_url_for(product, last_year, last_month)).call().map_err(|e| format!("download failed: {e}"))?,
    };

    let mut gz_bytes = Vec::new();
    resp.into_reader().read_to_end(&mut gz_bytes).map_err(|e| format!("read failed: {e}"))?;

    let mut decoder = flate2::read::GzDecoder::new(&gz_bytes[..]);
    let mut mmdb_bytes = Vec::new();
    decoder.read_to_end(&mut mmdb_bytes).map_err(|e| format!("gunzip failed: {e}"))?;

    maxminddb::Reader::from_source(mmdb_bytes.clone()).map_err(|e| format!("invalid database: {e}"))?;

    std::fs::create_dir_all(get_data_dir()).map_err(|e| e.to_string())?;
    std::fs::write(dest, &mmdb_bytes).map_err(|e| e.to_string())
}

/// Downloads the City database (required — this call fails if it does)
/// and the ASN database (best-effort — a failure here is logged but
/// doesn't fail the whole operation, since ASN is a bonus enrichment on
/// top of the core Geo-IP feature, not something the UI hard-depends
/// on: a missing ASN database just means the ASN column reads "—").
pub fn download_database() -> Result<(), String> {
    download_mmdb("city-lite", &database_path())?;
    if let Err(e) = download_mmdb("asn-lite", &asn_database_path()) {
        eprintln!("[network-monitor] ASN database download failed (city database is fine, ASN column will read '—' until this succeeds): {e}");
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct GeoLocation {
    pub city: Option<String>,
    pub country: Option<String>,
    pub lat: f64,
    pub lon: f64,
    /// Real ASN + organization name from the ASN database, independent
    /// of whether city data was found — `None` if the ASN database
    /// isn't downloaded, or has no entry for this address.
    pub asn: Option<u32>,
    pub asn_org: Option<String>,
}

/// Wraps the opened city/ASN readers (either or both may be `None`, if
/// not downloaded yet) so callers don't need to check `database_status()`
/// before every lookup.
pub struct GeoIpReader {
    city: Option<maxminddb::Reader<Vec<u8>>>,
    asn: Option<maxminddb::Reader<Vec<u8>>>,
}

impl GeoIpReader {
    /// Opens whichever databases are present; cheap to call again after
    /// `download_database()` succeeds to pick up the fresh file(s).
    pub fn open() -> Self {
        Self {
            city: maxminddb::Reader::open_readfile(database_path()).ok(),
            asn: maxminddb::Reader::open_readfile(asn_database_path()).ok(),
        }
    }

    /// Real lookups only — private/loopback/link-local addresses have no
    /// meaningful location and are skipped rather than guessed, and an
    /// address genuinely absent from a database leaves those fields
    /// `None` rather than a fabricated placeholder. Requires city data
    /// (lat/lon) to return anything at all, since this powers a map —
    /// ASN data is a purely additive enrichment on top of a city hit,
    /// not an alternate way to appear in the table.
    pub fn lookup(&self, ip_str: &str) -> Option<GeoLocation> {
        let ip: IpAddr = ip_str.parse().ok()?;
        if !is_public(&ip) {
            return None;
        }

        let city_reader = self.city.as_ref()?;
        let city: maxminddb::geoip2::City = city_reader.lookup(ip).ok()?.decode().ok()??;
        let lat = city.location.latitude?;
        let lon = city.location.longitude?;

        let (asn, asn_org) = self
            .asn
            .as_ref()
            .and_then(|r| r.lookup(ip).ok())
            .and_then(|r| r.decode::<maxminddb::geoip2::Asn>().ok())
            .flatten()
            .map(|a| (a.autonomous_system_number, a.autonomous_system_organization.map(str::to_string)))
            .unwrap_or((None, None));

        Some(GeoLocation {
            city: city.city.names.english.map(str::to_string),
            country: city.country.names.english.map(str::to_string),
            lat,
            lon,
            asn,
            asn_org,
        })
    }
}

fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_multicast() || v4.is_unspecified())
        }
        IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_multicast() || v6.is_unspecified()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_and_loopback_addresses_are_not_public() {
        assert!(!is_public(&"192.168.1.5".parse().unwrap()));
        assert!(!is_public(&"10.0.0.1".parse().unwrap()));
        assert!(!is_public(&"127.0.0.1".parse().unwrap()));
        assert!(!is_public(&"169.254.1.1".parse().unwrap()));
        assert!(is_public(&"8.8.8.8".parse().unwrap()));
        assert!(is_public(&"140.82.112.3".parse().unwrap()));
    }

    #[test]
    fn lookup_without_a_database_returns_none_not_a_panic() {
        // Neither database_path() nor asn_database_path() has a file in
        // this test environment by default — GeoIpReader::open() must
        // degrade to a working "always None" reader, not fail to
        // construct.
        let reader = GeoIpReader { city: None, asn: None };
        assert!(reader.lookup("8.8.8.8").is_none());
    }

    #[test]
    fn db_url_uses_zero_padded_month() {
        assert_eq!(db_url_for("city-lite", 2026, 1), "https://download.db-ip.com/free/dbip-city-lite-2026-01.mmdb.gz");
        assert_eq!(db_url_for("asn-lite", 2026, 12), "https://download.db-ip.com/free/dbip-asn-lite-2026-12.mmdb.gz");
    }
}

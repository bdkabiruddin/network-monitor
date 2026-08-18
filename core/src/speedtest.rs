use std::io::Cursor;
use std::time::{Duration, Instant};

/// Cloudflare's public speed-test endpoints — no API key required,
/// widely used by other open-source speed test tools (e.g. librespeed).
/// Real HTTP transfers timed end-to-end, not a simulation.
const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down";
const UPLOAD_URL: &str = "https://speed.cloudflare.com/__up";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

pub const DEFAULT_DOWNLOAD_TEST_BYTES: usize = 25_000_000;
pub const DEFAULT_UPLOAD_TEST_BYTES: usize = 8_000_000;

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(REQUEST_TIMEOUT).build()
}

/// Downloads `bytes` from Cloudflare's speed-test endpoint and returns
/// the measured throughput in Mbit/s.
pub fn run_download_mbps(bytes: usize) -> Result<f64, String> {
    let url = format!("{DOWNLOAD_URL}?bytes={bytes}");
    let start = Instant::now();
    let resp = agent().get(&url).call().map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    let copied = std::io::copy(&mut reader, &mut std::io::sink()).map_err(|e| e.to_string())?;
    let elapsed = start.elapsed().as_secs_f64().max(1e-6);
    Ok((copied as f64 * 8.0) / elapsed / 1_000_000.0)
}

/// Uploads `bytes` of a fixed pattern (a throughput test, not a
/// compression test, so randomness doesn't matter) to Cloudflare's
/// speed-test endpoint and returns the measured throughput in Mbit/s.
pub fn run_upload_mbps(bytes: usize) -> Result<f64, String> {
    let payload = vec![0x5A_u8; bytes];
    let start = Instant::now();
    agent()
        .post(UPLOAD_URL)
        .set("Content-Type", "application/octet-stream")
        .send(Cursor::new(payload))
        .map_err(|e| e.to_string())?;
    let elapsed = start.elapsed().as_secs_f64().max(1e-6);
    Ok((bytes as f64 * 8.0) / elapsed / 1_000_000.0)
}

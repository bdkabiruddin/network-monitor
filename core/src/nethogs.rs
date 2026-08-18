use std::process::Command;
use std::sync::OnceLock;

use regex::Regex;

pub const NETHOGS_SCAN_SECONDS: u32 = 10;

#[derive(Debug, Clone)]
pub struct NethogsRow {
    pub pid: Option<String>,
    pub program: String,
    pub sent: String,
    pub received: String,
}

fn pid_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"/(\d+)/\d+$").unwrap())
}

/// One line of `nethogs -t` trace-mode output:
/// "<program>/<pid>/<uid>\t<sent KB/s>\t<received KB/s>". Returns None if
/// the line doesn't match (blank lines between refresh cycles,
/// "Refreshing:" markers, etc).
pub fn parse_nethogs_trace_line(line: &str) -> Option<NethogsRow> {
    let parts: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
    if parts.len() != 3 {
        return None;
    }
    let (program_field, sent, received) = (parts[0], parts[1], parts[2]);
    let pid_match = pid_re().captures(program_field);
    // nethogs uses pid 0 for its "unknown TCP" bucket (traffic it can't
    // attribute to a process) — that's not a real PID to act on.
    let pid = pid_match.as_ref().and_then(|m| {
        let p = m.get(1).unwrap().as_str();
        (p != "0").then(|| p.to_string())
    });
    let program = match &pid_match {
        Some(m) => program_field[..m.get(0).unwrap().start()].trim_end_matches('/').to_string(),
        None => program_field.to_string(),
    };
    sent.parse::<f64>().ok()?;
    received.parse::<f64>().ok()?;
    Some(NethogsRow { pid, program, sent: format!("{sent} KB/s"), received: format!("{received} KB/s") })
}

/// Runs a fixed-duration NetHogs scan via pkexec (prompts the user for
/// an admin password) and returns the last complete trace-mode cycle,
/// sorted by sent+received descending. `timeout` sends SIGINT after
/// NETHOGS_SCAN_SECONDS so nethogs exits cleanly rather than running
/// forever.
pub fn run_nethogs_scan(limit: usize) -> Result<Vec<NethogsRow>, String> {
    let scan_cmd = format!("timeout --signal=INT {NETHOGS_SCAN_SECONDS} nethogs -t -d 1");
    let output = Command::new("pkexec")
        .args(["sh", "-c", &scan_cmd])
        .output()
        .map_err(|e| format!("couldn't start pkexec — is policykit installed? ({e})"))?;

    // `timeout` exits 124 on a normal timeout-triggered stop, or 130ish
    // once SIGINT propagates through nethogs's own exit code — either
    // way that's the *expected* end of a successful scan, not a
    // failure. Only exit codes it doesn't produce count as real errors.
    let code = output.status.code().unwrap_or(-1);
    if !matches!(code, 0 | 124 | 130 | 143) || output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().last().unwrap_or("").to_string();
        return Err(format!(
            "scan didn't complete — cancelled, or nethogs not installed (sudo apt install nethogs)? [{tail}]"
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let chunks: Vec<&str> = stdout.split("\n\n").filter(|c| !c.trim().is_empty()).collect();
    let cycle = if chunks.len() >= 2 { chunks[chunks.len() - 2] } else { chunks.last().copied().unwrap_or("") };

    let mut parsed: Vec<NethogsRow> = cycle.lines().filter_map(parse_nethogs_trace_line).collect();
    if parsed.is_empty() {
        return Err(format!("scan completed but no data parsed ({} bytes captured)", stdout.len()));
    }

    let sort_key = |row: &NethogsRow| -> f64 {
        let sent: f64 = row.sent.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        let recv: f64 = row.received.split_whitespace().next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
        sent + recv
    };
    parsed.sort_by(|a, b| sort_key(b).partial_cmp(&sort_key(a)).unwrap_or(std::cmp::Ordering::Equal));
    parsed.truncate(limit);
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_trace_line_with_pid() {
        let line = "/usr/bin/firefox/4242/1000\t12.34\t56.78";
        let row = parse_nethogs_trace_line(line).unwrap();
        assert_eq!(row.pid.as_deref(), Some("4242"));
        assert_eq!(row.program, "/usr/bin/firefox");
        assert_eq!(row.sent, "12.34 KB/s");
    }

    #[test]
    fn unknown_tcp_bucket_has_no_pid() {
        let line = "unknown TCP/0/0\t0.10\t0.00";
        let row = parse_nethogs_trace_line(line).unwrap();
        assert!(row.pid.is_none());
    }

    #[test]
    fn rejects_malformed_lines() {
        assert!(parse_nethogs_trace_line("Refreshing:").is_none());
        assert!(parse_nethogs_trace_line("").is_none());
    }
}

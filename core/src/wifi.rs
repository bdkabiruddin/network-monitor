use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NMCLI_FIELDS: &str = "IN-USE,SSID,BSSID,MODE,CHAN,FREQ,RATE,SIGNAL,BARS,SECURITY";

#[derive(Debug, Clone)]
pub struct WifiNetwork {
    pub in_use: bool,
    pub ssid: String,
    pub bssid: String,
    pub mode: String,
    pub channel: Option<i64>,
    pub freq_mhz: Option<i64>,
    pub rate_mbps: Option<i64>,
    pub signal_pct: Option<i64>,
    pub bars: String,
    pub security: String,
}

/// nmcli's terse (-t) output is colon-separated with literal colons
/// inside a field (BSSIDs, occasionally SSIDs) escaped as "\:" — a naive
/// `line.split(':')` would shred a BSSID like AA\:BB\:CC\:DD\:EE\:FF
/// into 6 extra bogus fields. Split only on unescaped colons, then
/// unescape. (The `regex` crate has no lookbehind support, so this is a
/// small hand-rolled scanner rather than the Python original's regex.)
/// Shared with `vpn` — any nmcli `-t` consumer needs the same parsing.
pub(crate) fn parse_nmcli_terse_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == ':' {
                    current.push(':');
                    chars.next();
                    continue;
                }
            }
            current.push(c);
        } else if c == ':' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

/// First device NetworkManager reports as type "wifi" — there's
/// normally at most one on a laptop, but nothing here assumes that.
pub fn get_wifi_interface() -> Option<String> {
    let output = Command::new("nmcli")
        .args(["-t", "-f", "DEVICE,TYPE", "dev", "status"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let fields = parse_nmcli_terse_line(line);
        if fields.len() >= 2 && fields[1] == "wifi" {
            return Some(fields[0].clone());
        }
    }
    None
}

fn leading_int(s: &str) -> Option<i64> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() { None } else { digits.parse().ok() }
}

fn to_int(s: &str) -> Option<i64> {
    s.parse().ok()
}

/// Shared by the synchronous poll and the async rescan path — same
/// `nmcli -f {NMCLI_FIELDS} dev wifi list` terse output either way.
pub fn parse_wifi_list_output(text: &str) -> Vec<WifiNetwork> {
    let mut networks = Vec::new();
    for line in text.lines() {
        let fields = parse_nmcli_terse_line(line);
        if fields.len() < 10 {
            continue;
        }
        let (in_use, ssid, bssid, mode, chan, freq, rate, signal, bars, security) = (
            &fields[0], &fields[1], &fields[2], &fields[3], &fields[4],
            &fields[5], &fields[6], &fields[7], &fields[8], &fields[9],
        );
        networks.push(WifiNetwork {
            in_use: in_use.trim() == "*",
            ssid: ssid.clone(),
            bssid: bssid.clone(),
            mode: mode.clone(),
            channel: to_int(chan),
            freq_mhz: leading_int(freq),
            rate_mbps: leading_int(rate),
            signal_pct: to_int(signal),
            bars: bars.clone(),
            security: if security.is_empty() { "Open".to_string() } else { security.clone() },
        });
    }
    networks.sort_by(|a, b| b.signal_pct.unwrap_or(-1).cmp(&a.signal_pct.unwrap_or(-1)));
    networks
}

/// Nearby access points via `nmcli dev wifi list`, one row per BSSID.
/// NetworkManager keeps its own scan cache and refreshes it periodically
/// on its own, so a plain call is normally near-instant; rescan=true
/// forces a fresh scan first (`--rescan yes`), which can take several
/// seconds.
pub fn read_wifi_networks(rescan: bool, timeout: Duration) -> Vec<WifiNetwork> {
    let mut args = vec!["-t", "-f", NMCLI_FIELDS, "dev", "wifi", "list"];
    if rescan {
        args.extend(["--rescan", "yes"]);
    }
    // std::process::Command has no built-in timeout, so `timeout` used to
    // be silently discarded -- a hung `nmcli` (unresponsive D-Bus/
    // NetworkManager) would block this call forever despite the 5s bound
    // callers thought they had. Polled via try_wait() instead of a
    // blocking wait, so it can be killed once `timeout` elapses.
    let Ok(mut child) = Command::new("nmcli").args(&args).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else {
        return Vec::new();
    };

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };

    let Some(status) = status else { return Vec::new() };
    if !status.success() {
        return Vec::new();
    }
    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    parse_wifi_list_output(&stdout)
}

pub fn get_active_wifi_network(networks: &[WifiNetwork]) -> Option<&WifiNetwork> {
    networks.iter().find(|n| n.in_use)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_unescaped_colons_only() {
        let line = r"*:MyNet:AA\:BB\:CC\:DD\:EE\:FF:Infra:6:2437:270:80:____:WPA2";
        let fields = parse_nmcli_terse_line(line);
        assert_eq!(fields[0], "*");
        assert_eq!(fields[1], "MyNet");
        assert_eq!(fields[2], "AA:BB:CC:DD:EE:FF");
        assert_eq!(fields[3], "Infra");
    }

    #[test]
    fn parses_full_wifi_list_line() {
        let text = r"*:HomeNet:AA\:BB\:CC\:DD\:EE\:FF:Infra:6:2437:270 Mbit/s:80:____:WPA2";
        let networks = parse_wifi_list_output(text);
        assert_eq!(networks.len(), 1);
        let n = &networks[0];
        assert!(n.in_use);
        assert_eq!(n.ssid, "HomeNet");
        assert_eq!(n.channel, Some(6));
        assert_eq!(n.freq_mhz, Some(2437));
        assert_eq!(n.rate_mbps, Some(270));
        assert_eq!(n.signal_pct, Some(80));
    }

    #[test]
    fn empty_security_defaults_to_open() {
        let text = ":OpenNet:AA\\:BB\\:CC\\:DD\\:EE\\:FF:Infra:1:2412:54:40:__:";
        let networks = parse_wifi_list_output(text);
        assert_eq!(networks[0].security, "Open");
    }
}

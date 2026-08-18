use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Interfaces excluded from `visible_interfaces` (and therefore from the
/// `total_downloaded`/`total_uploaded`/total-rate sums it feeds — the
/// Data Usage Cap figure among them) — loopback/virtual bridges because
/// they aren't real network traffic, and tunnel/VPN overlay interfaces
/// (tun/tap/wg/ppp) because their traffic already gets counted once by
/// whichever physical interface is actually carrying the (now encrypted)
/// packets. Without excluding the overlay, turning on a VPN roughly
/// doubles every total this feeds. Still counted in raw /proc/net/dev
/// parsing (`interfaces`, unfiltered) — this only affects the summed
/// totals, not the per-interface list UI.
const SKIP_IFACE_PREFIXES: &[&str] = &["lo", "virbr", "docker", "veth", "br-", "tun", "tap", "wg", "ppp"];

#[derive(Debug, Clone, Default)]
pub struct Counters {
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errs: u64,
    pub rx_drop: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errs: u64,
    pub tx_drop: u64,
}

#[derive(Debug, Clone, Default)]
pub struct IfaceRates {
    pub counters: Counters,
    pub rx_rate: f64,
    pub tx_rate: f64,
}

#[derive(Debug, Clone, Default)]
pub struct InterfaceInfo {
    pub address: Option<String>,
    pub mtu: Option<String>,
    pub operstate: String,
    pub speed_mbps: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct Reading {
    pub default_iface: Option<String>,
    pub interfaces: HashMap<String, IfaceRates>,
    pub visible_interfaces: HashMap<String, IfaceRates>,
    pub download_rate: f64,
    pub upload_rate: f64,
    pub total_download_rate: f64,
    pub total_upload_rate: f64,
    pub total_downloaded: f64,
    pub total_uploaded: f64,
    pub primary_info: InterfaceInfo,
}

/// Mirrors the Python reader's "pick the one that matters" pattern —
/// parses `ip route show default` for the interface actually carrying
/// traffic to the internet, rather than guessing from an alphabetical
/// interface list.
pub fn get_default_interface() -> Option<String> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if let Some(pos) = words.iter().position(|w| *w == "dev") {
            if let Some(name) = words.get(pos + 1) {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// All interfaces from /proc/net/dev, in the order the kernel lists them
/// (lo first, typically), unfiltered — callers decide what to hide.
pub fn list_interfaces() -> Vec<String> {
    let Ok(text) = fs::read_to_string("/proc/net/dev") else {
        return Vec::new();
    };
    text.lines()
        .skip(2)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Straight from the kernel's own cumulative counters (reset only on
/// reboot/interface reset) — same "always available, no root" property
/// /sys/class/power_supply has for battery stats.
pub fn read_proc_net_dev() -> HashMap<String, Counters> {
    let Ok(text) = fs::read_to_string("/proc/net/dev") else {
        return HashMap::new();
    };
    let mut stats = HashMap::new();
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        if fields.len() < 16 {
            continue;
        }
        let parse = |i: usize| fields[i].parse::<u64>().ok();
        if let (Some(rx_bytes), Some(rx_packets), Some(rx_errs), Some(rx_drop),
                Some(tx_bytes), Some(tx_packets), Some(tx_errs), Some(tx_drop)) = (
            parse(0), parse(1), parse(2), parse(3),
            parse(8), parse(9), parse(10), parse(11),
        ) {
            stats.insert(
                name.trim().to_string(),
                Counters { rx_bytes, rx_packets, rx_errs, rx_drop, tx_bytes, tx_packets, tx_errs, tx_drop },
            );
        }
    }
    stats
}

/// Address/MTU/operstate/speed straight from /sys/class/net/<iface>/.
/// speed is often unavailable (wireless interfaces don't report a link
/// speed the way wired ones do) — None rather than a fabricated number.
pub fn read_interface_info(iface: &str) -> InterfaceInfo {
    let base = Path::new("/sys/class/net").join(iface);
    let read = |name: &str| fs::read_to_string(base.join(name)).ok().map(|s| s.trim().to_string());

    let speed = read("speed").and_then(|s| s.parse::<i64>().ok());

    InterfaceInfo {
        address: read("address"),
        mtu: read("mtu"),
        operstate: read("operstate").unwrap_or_else(|| "unknown".to_string()),
        speed_mbps: speed.filter(|s| *s > 0),
    }
}

/// Wraps the raw /proc/net/dev cumulative counters into per-second rates.
/// Rates need a previous sample to diff against, so the first call after
/// construction (or after an interface appears/disappears) returns 0
/// rates rather than fabricating a number from a single point in time.
pub struct NetworkReader {
    last_sample: Option<HashMap<String, Counters>>,
    last_time: Option<Instant>,
}

impl NetworkReader {
    pub fn new() -> Self {
        Self { last_sample: None, last_time: None }
    }

    pub fn get(&mut self) -> Reading {
        let now = Instant::now();
        let raw = read_proc_net_dev();
        let default_iface = get_default_interface();
        let elapsed = self.last_time.map(|t| now.duration_since(t));

        let rates = compute_rates(&raw, self.last_sample.as_ref(), elapsed);

        self.last_sample = Some(raw);
        self.last_time = Some(now);

        let primary_info = default_iface.as_ref().map(|i| read_interface_info(i)).unwrap_or_default();

        build_reading(rates, default_iface, primary_info)
    }
}

/// Diffs `raw` against `prev` over `elapsed` to get a per-interface
/// rate. No `prev`/`elapsed` (first sample, or an interface that just
/// appeared) yields 0 rather than a fabricated number. A lower byte
/// count than the previous sample (counter reset — interface flap,
/// driver reload) is clamped to 0 via `max(0.0)` rather than reported as
/// a negative rate.
fn compute_rates(
    raw: &HashMap<String, Counters>,
    prev: Option<&HashMap<String, Counters>>,
    elapsed: Option<Duration>,
) -> HashMap<String, IfaceRates> {
    let mut rates: HashMap<String, IfaceRates> = HashMap::new();
    for (iface, counters) in raw {
        let prev_counters = prev.and_then(|m| m.get(iface));
        let (rx_rate, tx_rate) = match (prev_counters, elapsed) {
            (Some(prev), Some(elapsed)) if elapsed > Duration::ZERO => {
                let secs = elapsed.as_secs_f64();
                (
                    ((counters.rx_bytes as f64 - prev.rx_bytes as f64) / secs).max(0.0),
                    ((counters.tx_bytes as f64 - prev.tx_bytes as f64) / secs).max(0.0),
                )
            }
            _ => (0.0, 0.0),
        };
        rates.insert(iface.clone(), IfaceRates { counters: counters.clone(), rx_rate, tx_rate });
    }
    rates
}

/// Filters `rates` down to `visible_interfaces` (see `SKIP_IFACE_PREFIXES`
/// for what's excluded and why) and sums those into the total/primary
/// figures the Dashboard and Data Usage Cap read.
fn build_reading(rates: HashMap<String, IfaceRates>, default_iface: Option<String>, primary_info: InterfaceInfo) -> Reading {
    let visible: HashMap<String, IfaceRates> = rates
        .iter()
        .filter(|(name, _)| !SKIP_IFACE_PREFIXES.iter().any(|p| name.starts_with(p)))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let total_rx_rate: f64 = visible.values().map(|d| d.rx_rate).sum();
    let total_tx_rate: f64 = visible.values().map(|d| d.tx_rate).sum();
    let total_rx_bytes: f64 = visible.values().map(|d| d.counters.rx_bytes as f64).sum();
    let total_tx_bytes: f64 = visible.values().map(|d| d.counters.tx_bytes as f64).sum();

    let primary = default_iface.as_ref().and_then(|i| rates.get(i));

    Reading {
        download_rate: primary.map(|p| p.rx_rate).unwrap_or(total_rx_rate),
        upload_rate: primary.map(|p| p.tx_rate).unwrap_or(total_tx_rate),
        total_download_rate: total_rx_rate,
        total_upload_rate: total_tx_rate,
        total_downloaded: total_rx_bytes,
        total_uploaded: total_tx_bytes,
        primary_info,
        default_iface,
        interfaces: rates,
        visible_interfaces: visible,
    }
}

impl Default for NetworkReader {
    fn default() -> Self {
        Self::new()
    }
}

pub const LATENCY_TARGET: &str = "1.1.1.1";

/// Round-trip time in ms via a single ICMP echo, or None if the host is
/// unreachable/ping isn't available. Standard `ping` on Linux ships with
/// the CAP_NET_RAW file capability already set, so this works without
/// root or sudo.
pub fn measure_latency(host: &str, timeout_s: u64) -> Option<f64> {
    let output = Command::new("ping")
        .args(["-c", "1", "-W", &timeout_s.to_string(), host])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let idx = stdout.find("time=")?;
    let rest = &stdout[idx + 5..];
    let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dev_line_fields() {
        let stats = read_proc_net_dev();
        // No fixed assertions on content (machine-dependent), just that
        // parsing doesn't panic and returns a map keyed by iface name.
        for (name, c) in &stats {
            assert!(!name.is_empty());
            let _ = c.rx_bytes;
        }
    }

    fn counters(rx_bytes: u64, tx_bytes: u64) -> Counters {
        Counters { rx_bytes, tx_bytes, ..Default::default() }
    }

    #[test]
    fn compute_rates_diffs_against_previous_sample() {
        let mut prev = HashMap::new();
        prev.insert("wlan0".to_string(), counters(1_000, 500));
        let mut raw = HashMap::new();
        raw.insert("wlan0".to_string(), counters(3_000, 1_500));

        let rates = compute_rates(&raw, Some(&prev), Some(Duration::from_secs(2)));

        let r = &rates["wlan0"];
        assert_eq!(r.rx_rate, 1_000.0); // (3000-1000)/2s
        assert_eq!(r.tx_rate, 500.0); // (1500-500)/2s
    }

    #[test]
    fn compute_rates_clamps_a_counter_reset_to_zero_not_negative() {
        let mut prev = HashMap::new();
        prev.insert("wlan0".to_string(), counters(5_000, 5_000));
        let mut raw = HashMap::new();
        raw.insert("wlan0".to_string(), counters(100, 100)); // interface reset/flapped

        let rates = compute_rates(&raw, Some(&prev), Some(Duration::from_secs(1)));

        assert_eq!(rates["wlan0"].rx_rate, 0.0);
        assert_eq!(rates["wlan0"].tx_rate, 0.0);
    }

    #[test]
    fn compute_rates_is_zero_without_a_previous_sample() {
        let mut raw = HashMap::new();
        raw.insert("wlan0".to_string(), counters(3_000, 1_500));

        let rates = compute_rates(&raw, None, None);

        assert_eq!(rates["wlan0"].rx_rate, 0.0);
        assert_eq!(rates["wlan0"].tx_rate, 0.0);
    }

    #[test]
    fn build_reading_excludes_a_vpn_tunnel_from_the_total_so_it_is_not_double_counted() {
        let mut rates = HashMap::new();
        // The physical interface carrying the (now encrypted) traffic...
        rates.insert("wlan0".to_string(), IfaceRates { counters: counters(10_000, 2_000), rx_rate: 100.0, tx_rate: 20.0 });
        // ...and the VPN overlay riding on top of it, which must not add
        // its own bytes on top of wlan0's for the total.
        rates.insert("tun0".to_string(), IfaceRates { counters: counters(9_500, 1_900), rx_rate: 95.0, tx_rate: 19.0 });

        let reading = build_reading(rates, None, InterfaceInfo::default());

        assert_eq!(reading.total_downloaded, 10_000.0); // wlan0 only, not 19_500
        assert_eq!(reading.total_uploaded, 2_000.0);
        assert!(!reading.visible_interfaces.contains_key("tun0"));
        assert!(reading.interfaces.contains_key("tun0")); // still present, unfiltered
    }

    #[test]
    fn build_reading_uses_the_default_interface_as_primary_when_present() {
        let mut rates = HashMap::new();
        rates.insert("wlan0".to_string(), IfaceRates { counters: counters(1_000, 500), rx_rate: 50.0, tx_rate: 10.0 });
        rates.insert("eth0".to_string(), IfaceRates { counters: counters(2_000, 1_000), rx_rate: 200.0, tx_rate: 40.0 });

        let reading = build_reading(rates, Some("eth0".to_string()), InterfaceInfo::default());

        assert_eq!(reading.download_rate, 200.0);
        assert_eq!(reading.upload_rate, 40.0);
    }
}

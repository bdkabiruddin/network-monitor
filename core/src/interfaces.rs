use std::path::Path;
use std::process::Command;

use crate::netdev::{get_default_interface, list_interfaces, read_interface_info, read_proc_net_dev};

#[derive(Debug, Clone)]
pub struct InterfaceSummary {
    pub name: String,
    pub kind: &'static str,
    pub ipv4: Option<String>,
    pub mac: Option<String>,
    pub mtu: Option<String>,
    pub operstate: String,
    pub speed_mbps: Option<i64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub is_default: bool,
}

/// Best-effort classification from what sysfs actually exposes: a
/// `wireless` subdirectory means the kernel driver identifies it as an
/// 802.11 device; there's no equivalently reliable marker for "this is
/// definitely Ethernet", so anything else non-loopback is reported as
/// Ethernet/other rather than guessed from the interface name.
fn classify(name: &str) -> &'static str {
    classify_under(Path::new("/sys/class/net"), name)
}

fn classify_under(base: &Path, name: &str) -> &'static str {
    if name == "lo" {
        "Loopback"
    } else if base.join(name).join("wireless").exists() {
        "Wi-Fi"
    } else {
        "Ethernet"
    }
}

fn read_ipv4(name: &str) -> Option<String> {
    let output = Command::new("ip").args(["-o", "-4", "addr", "show", name]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let words: Vec<&str> = stdout.split_whitespace().collect();
    let idx = words.iter().position(|w| *w == "inet")?;
    words.get(idx + 1).map(|s| s.to_string())
}

/// Every interface the kernel knows about (not just the default route),
/// for the Interfaces screen — real data from /proc/net/dev,
/// /sys/class/net, and `ip addr`, nothing simulated.
pub fn list_all_interfaces() -> Vec<InterfaceSummary> {
    let default_iface = get_default_interface();
    let counters = read_proc_net_dev();

    list_interfaces()
        .into_iter()
        .map(|name| {
            let info = read_interface_info(&name);
            let c = counters.get(&name);
            InterfaceSummary {
                kind: classify(&name),
                ipv4: read_ipv4(&name),
                mac: info.address,
                mtu: info.mtu,
                operstate: info.operstate,
                speed_mbps: info.speed_mbps,
                rx_bytes: c.map(|c| c.rx_bytes).unwrap_or(0),
                tx_bytes: c.map(|c| c.tx_bytes).unwrap_or(0),
                is_default: default_iface.as_deref() == Some(name.as_str()),
                name,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct BondGroup {
    pub name: String,
    pub mode: String,
    pub active_slave: Option<String>,
    pub slaves: Vec<String>,
}

/// Bonding/failover groups from /proc/net/bonding/*, if any are
/// configured — an empty list (not fake data) when bonding isn't set up,
/// which is the common case on a laptop.
pub fn list_bond_groups() -> Vec<BondGroup> {
    let Ok(entries) = std::fs::read_dir("/proc/net/bonding") else {
        return Vec::new();
    };
    let mut groups = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
        groups.push(parse_bond_info(name, &text));
    }
    groups
}

/// Parses a single /proc/net/bonding/<name> file's contents. Split out
/// from `list_bond_groups` so the parsing itself is testable without a
/// real bonding interface configured.
fn parse_bond_info(name: String, text: &str) -> BondGroup {
    let mode = text
        .lines()
        .find(|l| l.starts_with("Bonding Mode:"))
        .map(|l| l.trim_start_matches("Bonding Mode:").trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let active_slave = text
        .lines()
        .find(|l| l.starts_with("Currently Active Slave:"))
        .map(|l| l.trim_start_matches("Currently Active Slave:").trim().to_string())
        .filter(|s| !s.is_empty() && s != "None");
    let slaves = text
        .lines()
        .filter(|l| l.starts_with("Slave Interface:"))
        .map(|l| l.trim_start_matches("Slave Interface:").trim().to_string())
        .collect();

    BondGroup { name, mode, active_slave, slaves }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_reports_loopback_by_name_without_touching_the_filesystem() {
        assert_eq!(classify_under(Path::new("/does/not/exist"), "lo"), "Loopback");
    }

    #[test]
    fn classify_reports_wifi_when_the_wireless_subdir_exists() {
        let base = std::env::temp_dir().join(format!("netmon-test-classify-wifi-{}", std::process::id()));
        std::fs::create_dir_all(base.join("wlan0").join("wireless")).unwrap();

        let kind = classify_under(&base, "wlan0");
        std::fs::remove_dir_all(&base).ok();

        assert_eq!(kind, "Wi-Fi");
    }

    #[test]
    fn classify_falls_back_to_ethernet_without_a_wireless_subdir() {
        let base = std::env::temp_dir().join(format!("netmon-test-classify-eth-{}", std::process::id()));
        std::fs::create_dir_all(base.join("eth0")).unwrap();

        let kind = classify_under(&base, "eth0");
        std::fs::remove_dir_all(&base).ok();

        assert_eq!(kind, "Ethernet");
    }

    #[test]
    fn parse_bond_info_extracts_mode_active_slave_and_all_slaves() {
        let text = "\
Ethernet Channel Bonding Driver: v6.6.0

Bonding Mode: fault-tolerance (active-backup)
Currently Active Slave: eth0
MII Status: up

Slave Interface: eth0
MII Status: up

Slave Interface: eth1
MII Status: backup
";
        let group = parse_bond_info("bond0".to_string(), text);

        assert_eq!(group.name, "bond0");
        assert_eq!(group.mode, "fault-tolerance (active-backup)");
        assert_eq!(group.active_slave.as_deref(), Some("eth0"));
        assert_eq!(group.slaves, vec!["eth0".to_string(), "eth1".to_string()]);
    }

    #[test]
    fn parse_bond_info_treats_none_active_slave_as_absent() {
        let text = "Bonding Mode: load balancing (round-robin)\nCurrently Active Slave: None\n";
        let group = parse_bond_info("bond1".to_string(), text);
        assert_eq!(group.active_slave, None);
    }
}

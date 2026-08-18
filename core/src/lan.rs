use std::process::Command;

#[derive(Debug, Clone)]
pub struct LanDevice {
    pub ip: String,
    pub mac: Option<String>,
    pub iface: Option<String>,
    pub state: String,
}

fn parse_neighbor_line(line: &str) -> Option<LanDevice> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let ip = (*tokens.first()?).to_string();
    let state = (*tokens.last()?).to_string();

    let mut iface = None;
    let mut mac = None;
    let mut i = 1;
    while i < tokens.len() {
        match tokens[i] {
            "dev" if i + 1 < tokens.len() => {
                iface = Some(tokens[i + 1].to_string());
                i += 2;
            }
            "lladdr" if i + 1 < tokens.len() => {
                mac = Some(tokens[i + 1].to_string());
                i += 2;
            }
            _ => i += 1,
        }
    }
    Some(LanDevice { ip, mac, iface, state })
}

/// Devices this machine has an ARP/NDP cache entry for, via
/// `ip neighbor show` — real, no active discovery/scanning. No bundled
/// IEEE OUI database, so there's no vendor column here; guessing a
/// vendor from a MAC prefix without a real lookup table would be
/// fabricated data.
pub fn list_lan_devices() -> Vec<LanDevice> {
    let Ok(output) = Command::new("ip").args(["neighbor", "show"]).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout).lines().filter_map(parse_neighbor_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_line_with_lladdr() {
        let d = parse_neighbor_line("192.168.0.31 dev wlp0s20f3 lladdr 8c:c5:d0:cd:ef:52 STALE").unwrap();
        assert_eq!(d.ip, "192.168.0.31");
        assert_eq!(d.iface.as_deref(), Some("wlp0s20f3"));
        assert_eq!(d.mac.as_deref(), Some("8c:c5:d0:cd:ef:52"));
        assert_eq!(d.state, "STALE");
    }

    #[test]
    fn parses_line_without_lladdr() {
        let d = parse_neighbor_line("169.254.169.254 dev virbr0  FAILED").unwrap();
        assert_eq!(d.ip, "169.254.169.254");
        assert_eq!(d.iface.as_deref(), Some("virbr0"));
        assert!(d.mac.is_none());
        assert_eq!(d.state, "FAILED");
    }
}

use std::collections::HashSet;
use std::process::Command;

use crate::wifi::parse_nmcli_terse_line;

#[derive(Debug, Clone)]
pub struct VpnProfile {
    pub name: String,
    pub vpn_type: String,
    pub active: bool,
}

fn is_vpn_type(ty: &str) -> bool {
    ty == "vpn" || ty.contains("wireguard")
}

fn nmcli_connections(active_only: bool) -> Vec<(String, String)> {
    let mut args = vec!["-t", "-f", "NAME,TYPE", "connection", "show"];
    if active_only {
        args.push("--active");
    }
    let Ok(output) = Command::new("nmcli").args(&args).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let fields = parse_nmcli_terse_line(line);
            (fields.len() >= 2).then(|| (fields[0].clone(), fields[1].clone()))
        })
        .collect()
}

/// VPN connection profiles NetworkManager knows about (`nmcli connection
/// show`, filtered to the `vpn`/WireGuard connection types), each
/// flagged with whether it's currently active. Real profiles the user
/// configured — empty if none exist, nothing fabricated.
pub fn list_vpn_profiles() -> Vec<VpnProfile> {
    build_vpn_profiles(&nmcli_connections(true), &nmcli_connections(false))
}

/// Cross-references the active-only and all-connections lists into
/// `VpnProfile`s, filtered to VPN types. Split out from
/// `list_vpn_profiles` so this logic is testable without shelling out to
/// `nmcli`.
fn build_vpn_profiles(active_conns: &[(String, String)], all_conns: &[(String, String)]) -> Vec<VpnProfile> {
    let active: HashSet<&str> =
        active_conns.iter().filter(|(_, ty)| is_vpn_type(ty)).map(|(name, _)| name.as_str()).collect();

    all_conns
        .iter()
        .filter(|(_, ty)| is_vpn_type(ty))
        .map(|(name, vpn_type)| VpnProfile { name: name.clone(), vpn_type: vpn_type.clone(), active: active.contains(name.as_str()) })
        .collect()
}

/// Brings a VPN connection up via `nmcli connection up <name>`. Can
/// block for several seconds (handshake) or prompt via NetworkManager's
/// own secret agent if no credentials are cached — callers must run
/// this off the UI thread.
pub fn connect(name: &str) -> Result<(), String> {
    let output = Command::new("nmcli").args(["connection", "up", name]).output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

pub fn disconnect(name: &str) -> Result<(), String> {
    let output = Command::new("nmcli").args(["connection", "down", name]).output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_vpn_type_matches_vpn_and_wireguard_not_ordinary_connections() {
        assert!(is_vpn_type("vpn"));
        assert!(is_vpn_type("wireguard"));
        assert!(!is_vpn_type("802-11-wireless"));
        assert!(!is_vpn_type("ethernet"));
    }

    #[test]
    fn build_vpn_profiles_flags_active_and_filters_non_vpn_types() {
        let active = vec![("work".to_string(), "vpn".to_string())];
        let all = vec![
            ("work".to_string(), "vpn".to_string()),
            ("home-wg".to_string(), "wireguard".to_string()),
            ("Home Wi-Fi".to_string(), "802-11-wireless".to_string()),
        ];

        let profiles = build_vpn_profiles(&active, &all);

        assert_eq!(profiles.len(), 2); // the wireless connection is filtered out
        let work = profiles.iter().find(|p| p.name == "work").unwrap();
        assert!(work.active);
        let home = profiles.iter().find(|p| p.name == "home-wg").unwrap();
        assert!(!home.active);
    }
}

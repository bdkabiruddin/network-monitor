use std::path::PathBuf;

/// XDG autostart entry, user-scoped (`~/.config/autostart`) — no root
/// needed, unlike the `.deb` package's `/etc/xdg/autostart` entry.
fn autostart_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
    Some(config.join("autostart").join("network-monitor.desktop"))
}

pub fn is_enabled() -> bool {
    autostart_path().is_some_and(|p| p.exists())
}

/// Points the autostart entry at the currently running binary — during
/// development that's `target/debug/network-monitor`, not an installed
/// path, which is the honest thing to do until packaging (see
/// ROADMAP.md Phase 7) gives it a stable installed location.
pub fn enable() -> Result<(), String> {
    let path = autostart_path().ok_or("couldn't determine XDG config directory")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let contents = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Network Monitor\n\
         Comment=Live network bandwidth, connections and history\n\
         Exec={}\n\
         Terminal=false\n\
         Categories=Utility;\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::write(&path, contents).map_err(|e| e.to_string())
}

pub fn disable() -> Result<(), String> {
    let Some(path) = autostart_path() else {
        return Ok(());
    };
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_then_disable_round_trips_the_autostart_entry() {
        let dir = std::env::temp_dir().join(format!("netmon-test-autostart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        // SAFETY: no other test in this crate reads or writes
        // XDG_CONFIG_HOME, so there's nothing else to race with.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };

        assert!(!is_enabled(), "shouldn't be enabled before enable() is ever called");

        enable().unwrap();
        assert!(is_enabled());
        let contents = std::fs::read_to_string(dir.join("autostart").join("network-monitor.desktop")).unwrap();
        assert!(contents.contains("Type=Application"));
        assert!(contents.contains("Exec="));

        disable().unwrap();
        assert!(!is_enabled());

        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}

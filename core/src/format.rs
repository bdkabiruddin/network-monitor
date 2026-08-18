/// Bandwidth, not storage — bits would be conventional, but bytes matches
/// what /proc/net/dev actually reports and avoids a confusing x8 mental
/// conversion for anyone cross-checking against the raw counters.
pub fn format_bytes_rate(bytes_per_sec: Option<f64>) -> String {
    let Some(mut value) = bytes_per_sec else {
        return "—".to_string();
    };
    for unit in ["B/s", "KB/s", "MB/s"] {
        if value < 1024.0 {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{value:.1} GB/s")
}

pub fn format_bytes_total(total_bytes: Option<f64>) -> String {
    let Some(mut value) = total_bytes else {
        return "—".to_string();
    };
    for unit in ["B", "KB", "MB", "GB"] {
        if value < 1024.0 {
            return format!("{value:.2} {unit}");
        }
        value /= 1024.0;
    }
    format!("{value:.2} TB")
}

/// "2m 15s" / "1h 04m" / "3d 02h" — biggest two non-zero units, like most
/// "time ago"-style displays. `None` (or a negative value, which
/// shouldn't happen but isn't worth a panic over) renders as "—".
pub fn format_duration(seconds: Option<f64>) -> String {
    let Some(total) = seconds.filter(|s| *s >= 0.0) else {
        return "—".to_string();
    };
    let total = total.round() as u64;
    let (days, rem) = (total / 86400, total % 86400);
    let (hours, rem) = (rem / 3600, rem % 3600);
    let (mins, secs) = (rem / 60, rem % 60);
    if days > 0 {
        format!("{days}d {hours:02}h")
    } else if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else if mins > 0 {
        format!("{mins}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_formatting_steps_units() {
        assert_eq!(format_bytes_rate(None), "—");
        assert_eq!(format_bytes_rate(Some(0.0)), "0.0 B/s");
        assert_eq!(format_bytes_rate(Some(2048.0)), "2.0 KB/s");
        assert_eq!(format_bytes_rate(Some(5.0 * 1024.0 * 1024.0)), "5.0 MB/s");
    }

    #[test]
    fn total_formatting_steps_units() {
        assert_eq!(format_bytes_total(None), "—");
        assert_eq!(format_bytes_total(Some(1536.0)), "1.50 KB");
    }

    #[test]
    fn duration_formatting_picks_the_two_biggest_units() {
        assert_eq!(format_duration(None), "—");
        assert_eq!(format_duration(Some(-1.0)), "—");
        assert_eq!(format_duration(Some(5.0)), "5s");
        assert_eq!(format_duration(Some(135.0)), "2m 15s");
        assert_eq!(format_duration(Some(3600.0 + 4.0 * 60.0)), "1h 04m");
        assert_eq!(format_duration(Some(2.0 * 86400.0 + 2.0 * 3600.0)), "2d 02h");
    }
}

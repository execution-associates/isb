//! Small text renderings: sparklines, bars, sizes, ages.

/// An inline sparkline of the last `width` values, scaled to `max` (or to
/// the largest value when `max` is None).
pub fn spark(values: &[f32], width: usize, max: Option<f32>) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if width == 0 {
        return String::new();
    }
    let tail = &values[values.len().saturating_sub(width)..];
    let top = max
        .unwrap_or_else(|| tail.iter().copied().fold(0.0, f32::max))
        .max(f32::EPSILON);
    let mut s: String = std::iter::repeat_n(' ', width - tail.len()).collect();
    for v in tail {
        let i = ((v / top).clamp(0.0, 1.0) * 7.0).round() as usize;
        s.push(BARS[i]);
    }
    s
}

/// A horizontal bar `▕████░░░░▏` filled to `frac`.
pub fn bar(frac: f64, width: usize) -> String {
    let inner = width.saturating_sub(2);
    let eighths = (frac.clamp(0.0, 1.0) * inner as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let part = eighths % 8;
    const PART: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let mut s = String::from("▕");
    s.extend(std::iter::repeat_n('█', full));
    if full < inner {
        if part > 0 {
            s.push(PART[part]);
        } else {
            s.push('░');
        }
        s.extend(std::iter::repeat_n('░', inner - full - 1));
    }
    s.push('▏');
    s
}

/// `84M`, `1.2G`, `251G`.
pub fn bytes(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b < K * K {
        format!("{:.0}K", b / K)
    } else if b < K * K * K {
        format!("{:.0}M", b / K / K)
    } else if b < 10.0 * K * K * K {
        format!("{:.1}G", b / K / K / K)
    } else {
        format!("{:.0}G", b / K / K / K)
    }
}

/// `12s`, `4m`, `3h`, `2d` since `then` (unix seconds).
pub fn age(then: u64, now: u64) -> String {
    if then == 0 {
        return "-".into();
    }
    let d = now.saturating_sub(then);
    match d {
        0..=59 => format!("{d}s"),
        60..=3599 => format!("{}m", d / 60),
        3600..=86399 => format!("{}h", d / 3600),
        _ => format!("{}d", d / 86400),
    }
}

/// `HH:MM:SS` local time of a unix-ms timestamp.
pub fn clock(ms: u64) -> String {
    let secs = (ms / 1000) as i64 + local_offset();
    let d = secs.rem_euclid(86400);
    format!("{:02}:{:02}:{:02}", d / 3600, d % 3600 / 60, d % 60)
}

/// The local UTC offset in seconds, asked of `date` once: the standard
/// library has no time zones, and a dashboard clock in UTC reads wrong.
fn local_offset() -> i64 {
    static OFF: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                let s = s.trim();
                let sign = if s.starts_with('-') { -1 } else { 1 };
                let n: i64 = s.trim_start_matches(['+', '-']).parse().ok()?;
                Some(sign * (n / 100 * 3600 + n % 100 * 60))
            })
            .unwrap_or(0)
    })
}

/// Truncate to `w` columns with an ellipsis.
pub fn trunc(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(w - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparks_and_bars() {
        assert_eq!(spark(&[0.0, 50.0, 100.0], 3, Some(100.0)), "▁▅█");
        assert_eq!(spark(&[1.0], 3, None), "  █");
        assert_eq!(bar(0.0, 6), "▕░░░░▏");
        assert_eq!(bar(1.0, 6), "▕████▏");
        assert_eq!(bar(0.5, 6), "▕██░░▏");
        assert_eq!(bytes(84 * 1024 * 1024), "84M");
        assert_eq!(bytes(251 * 1024 * 1024 * 1024), "251G");
        assert_eq!(age(100, 160), "1m");
        assert_eq!(trunc("abcdef", 4), "abc…");
    }
}

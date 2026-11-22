//! Numbers for people (GUI-SPEC §6.6): `12,480`, `1.4 MB`, `320 ms`, `2 min 05 s`.

use std::time::Duration;

/// A count with thousands separators: `12480` → `12,480`.
pub fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A counted noun: `1 option`, `5 options`, `12,480 URLs` (regular plurals only).
pub fn plural(value: u64, noun: &str) -> String {
    let s = if value == 1 { "" } else { "s" };
    format!("{} {noun}{s}", count(value))
}

/// A size in bytes: `512 B`, `12.3 KB`, `1.4 MB`, `2.05 GB` (binary units).
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if value < 1024 {
        return format!("{value} B");
    }
    let mut size = value as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    let text = if size < 10.0 {
        format!("{size:.2}")
    } else if size < 100.0 {
        format!("{size:.1}")
    } else {
        format!("{size:.0}")
    };
    format!("{text} {}", UNITS[unit])
}

/// A duration given in milliseconds: `320 ms`, `4.2 s`, `2 min 05 s`, `1 h 02 min`.
pub fn millis(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms} ms");
    }
    if ms < 60_000 {
        let seconds = ms as f64 / 1000.0;
        return if seconds < 10.0 {
            format!("{seconds:.1} s")
        } else {
            format!("{} s", ms / 1000)
        };
    }
    let total_seconds = ms / 1000;
    let (hours, minutes, seconds) = (total_seconds / 3600, (total_seconds / 60) % 60, total_seconds % 60);
    if hours > 0 {
        format!("{hours} h {minutes:02} min")
    } else {
        format!("{minutes} min {seconds:02} s")
    }
}

/// A [`Duration`] as [`millis`].
pub fn duration(value: Duration) -> String {
    millis(u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_get_thousands_separators() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(12_480), "12,480");
        assert_eq!(count(1_234_567), "1,234,567");
    }

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.00 KB");
        assert_eq!(bytes(1_468_006), "1.40 MB");
        assert_eq!(bytes(50 * 1024 * 1024), "50.0 MB");
        assert_eq!(bytes(300 * 1024 * 1024), "300 MB");
        assert_eq!(bytes(u64::MAX), "16384 PB");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(millis(320), "320 ms");
        assert_eq!(millis(4_200), "4.2 s");
        assert_eq!(millis(42_000), "42 s");
        assert_eq!(millis(125_000), "2 min 05 s");
        assert_eq!(millis(3_720_000), "1 h 02 min");
        assert_eq!(duration(Duration::from_millis(1500)), "1.5 s");
    }
}

//! Duration parsing for shell-style time strings.
//!
//! Used by the `timeout` builtin and `scatter --timeout` to parse durations
//! like `30`, `30s`, `500ms`, `5m`, `1h`.

use std::time::Duration;

/// Parse a duration string: `30` (seconds), `30s`, `500ms`, `5m`, `1h`.
///
/// Returns `None` for invalid input (negative, unrecognized suffix, non-numeric).
pub fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();

    if let Ok(secs) = s.parse::<f64>() {
        return if secs >= 0.0 {
            Some(Duration::from_secs_f64(secs))
        } else {
            None
        };
    }

    if let Some(num) = s.strip_suffix("ms") {
        let ms: u64 = num.trim().parse().ok()?;
        return Some(Duration::from_millis(ms));
    }
    if let Some(num) = s.strip_suffix('s') {
        let secs: f64 = num.trim().parse().ok()?;
        return if secs >= 0.0 {
            Some(Duration::from_secs_f64(secs))
        } else {
            None
        };
    }
    if let Some(num) = s.strip_suffix('m') {
        let mins: f64 = num.trim().parse().ok()?;
        return if mins >= 0.0 {
            Some(Duration::from_secs_f64(mins * 60.0))
        } else {
            None
        };
    }
    if let Some(num) = s.strip_suffix('h') {
        let hours: f64 = num.trim().parse().ok()?;
        return if hours >= 0.0 {
            Some(Duration::from_secs_f64(hours * 3600.0))
        } else {
            None
        };
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds() {
        assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("0"), Some(Duration::from_secs(0)));
        assert_eq!(parse_duration("1.5"), Some(Duration::from_secs_f64(1.5)));
    }

    #[test]
    fn suffixes() {
        assert_eq!(parse_duration("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn invalid() {
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("-5"), None);
        assert_eq!(parse_duration("5x"), None);
    }
}

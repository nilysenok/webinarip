//! Time strings for `--from/--to`: `75`, `01:15`, `1:02:03`, `1h2m3s`, `90m`, `45s`.

use crate::{Error, Result};

/// Parses a time string into seconds. `MM:SS` and `H:MM:SS` follow the ffmpeg convention.
pub fn parse(s: &str) -> Result<f64> {
    let s = s.trim();
    let bad = || Error::Usage(format!("cannot read time «{s}»: use 75, 01:15, 1:02:03 or 1h2m3s"));
    if s.contains(':') {
        let parts: Vec<f64> = s.split(':').map(|p| p.parse::<f64>().map_err(|_| bad())).collect::<Result<_>>()?;
        if parts.len() > 3 {
            return Err(bad());
        }
        return Ok(parts.iter().fold(0.0, |acc, p| acc * 60.0 + p));
    }
    if s.ends_with(['h', 'm', 's']) {
        let (mut total, mut num) = (0.0, String::new());
        for c in s.chars() {
            match c {
                '0'..='9' | '.' => num.push(c),
                'h' | 'm' | 's' => {
                    let v: f64 = num.parse().map_err(|_| bad())?;
                    total += v * match c {
                        'h' => 3600.0,
                        'm' => 60.0,
                        _ => 1.0,
                    };
                    num.clear();
                }
                _ => return Err(bad()),
            }
        }
        return if num.is_empty() { Ok(total) } else { Err(bad()) };
    }
    s.parse().map_err(|_| bad())
}

/// `3725.4` → `1:02:05`.
pub fn hms(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(parse("75").unwrap(), 75.0);
        assert_eq!(parse("01:15").unwrap(), 75.0);
        assert_eq!(parse("1:02:03").unwrap(), 3723.0);
        assert_eq!(parse("1h2m3s").unwrap(), 3723.0);
        assert_eq!(parse("90m").unwrap(), 5400.0);
        assert!(parse("1:2:3:4").is_err());
        assert!(parse("abc").is_err());
        assert_eq!(hms(3723.0), "1:02:03");
        assert_eq!(hms(75.0), "1:15");
    }
}

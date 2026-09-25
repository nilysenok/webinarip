//! Time window and output location: every run gets its own dated folder.

use std::path::{Path, PathBuf};

use crate::job::Options;
use crate::record::Record;
use crate::{Error, Result, timefmt};

pub fn window(opts: &Options, rec: &Record) -> Result<(f64, f64)> {
    let from = opts.from.unwrap_or(0.0).max(0.0);
    let to = opts.to.unwrap_or(rec.duration).min(rec.duration);
    if to <= from {
        return Err(Error::Usage(format!(
            "empty range: from {} to {} (recording is {})",
            timefmt::hms(from),
            timefmt::hms(to),
            timefmt::hms(rec.duration)
        )));
    }
    Ok((from, to))
}

pub fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "recording".into() } else { s }
}

/// `<out>/YYYY-MM-DD_HHMM <title>/<title>[ range].<ext>` — a new folder for every run.
pub fn output_path(out_dir: &Path, rec: &Record, range: Option<(f64, f64)>, ext: &str) -> Result<PathBuf> {
    let base = format!("{} {}", chrono::Local::now().format("%Y-%m-%d_%H%M"), sanitize(&rec.title));
    let mut dir = out_dir.join(&base);
    for n in 2.. {
        if !dir.exists() {
            break;
        }
        dir = out_dir.join(format!("{base} ({n})"));
    }
    std::fs::create_dir_all(&dir)?;
    let span = range
        .map(|(a, b)| format!(" [{}–{}]", timefmt::hms(a), timefmt::hms(b)).replace(':', "-"))
        .unwrap_or_default();
    Ok(dir.join(format!("{}{span}.{ext}", sanitize(&rec.title))))
}

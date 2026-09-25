//! On-disk segment cache keyed by recording id: changing the format, the time range or simply
//! running again never downloads a segment twice.
//!
//! Layout: `<root>/<record id>/<track id>/<kind>/init.mp4` and `…/<kind>/<index>.m4s`.

use std::path::{Path, PathBuf};

pub fn default_root() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("webinarip")
}

pub fn track_dir(root: &Path, record: &str, track: u64, kind: &str) -> PathBuf {
    root.join(record).join(track.to_string()).join(kind)
}

pub fn init_path(dir: &Path) -> PathBuf {
    dir.join("init.mp4")
}

pub fn segment_path(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("{index:05}.m4s"))
}

/// Write via a temporary file and rename: a crash never leaves a half-written segment
/// under its final name, so presence of the file means it is complete.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, data)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_leaves_no_part_file() {
        let dir = std::env::temp_dir().join(format!("webinarip-test-{}", std::process::id()));
        let p = segment_path(&track_dir(&dir, "1", 2, "a"), 7);
        write_atomic(&p, b"abc").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"abc");
        assert!(p.ends_with("1/2/a/00007.m4s"));
        assert!(!p.with_extension("part").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

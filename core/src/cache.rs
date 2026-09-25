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
/// under its final name, so presence of the file means it is complete. Every writer gets its
/// own temporary name — a rushed duplicate and the original request may finish together.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.part", std::process::id()));
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
        let leftovers = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "part"));
        assert_eq!(leftovers.count(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn concurrent_writers_of_one_segment_both_succeed() {
        let dir = std::env::temp_dir().join(format!("webinarip-race-{}", std::process::id()));
        let p = segment_path(&dir, 1);
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let p = p.clone();
                std::thread::spawn(move || (0..200).map(|_| write_atomic(&p, b"same bytes")).all(|r| r.is_ok()))
            })
            .collect();
        assert!(threads.into_iter().all(|t| t.join().unwrap()));
        assert_eq!(std::fs::read(&p).unwrap(), b"same bytes");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

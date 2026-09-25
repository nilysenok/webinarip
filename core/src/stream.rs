//! Reading a track while it is still downloading: the files are read back to back as one
//! forward-only stream, waiting for each file to appear; the wait is reported to the downloader.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use symphonia::core::io::MediaSource;

/// Download state shared with decoders that read files while they are still arriving.
#[derive(Default)]
pub struct Gate {
    pub finished: AtomicBool,
    pub failed: AtomicBool,
    /// Files a decoder is blocked on right now, and since when — the downloader rushes them.
    waiting: Mutex<Vec<(PathBuf, Instant)>>,
    /// Longest time any decoder was blocked on one file, milliseconds.
    longest_ms: AtomicU64,
}

impl Gate {
    fn wait_on(&self, path: &Path) {
        let mut w = self.waiting.lock().unwrap();
        if !w.iter().any(|(p, _)| p == path) {
            w.push((path.to_path_buf(), Instant::now()));
        }
    }

    fn done_waiting(&self, path: &Path) {
        let mut w = self.waiting.lock().unwrap();
        if let Some(pos) = w.iter().position(|(p, _)| p == path) {
            let (_, since) = w.swap_remove(pos);
            self.longest_ms.fetch_max(since.elapsed().as_millis() as u64, Relaxed);
        }
    }

    /// The longest the mixer ever waited for a single segment — head-of-line blocking.
    pub fn longest_wait(&self) -> Duration {
        Duration::from_millis(self.longest_ms.load(Relaxed))
    }

    /// What the mixer is waiting for, with how long it has been waiting.
    pub fn waited(&self) -> Vec<(PathBuf, Duration)> {
        self.waiting.lock().unwrap().iter().map(|(p, t)| (p.clone(), t.elapsed())).collect()
    }
}

/// The track's files read back to back as one forward-only stream. A file that is not on
/// disk yet is waited for: segments are written atomically, so once a file is visible it is
/// complete. This lets decoding and mixing run while the download is still going.
pub struct StreamSource {
    files: Vec<PathBuf>,
    next: usize,
    open: Option<File>,
    gate: Arc<Gate>,
}

impl StreamSource {
    pub fn new(files: Vec<PathBuf>, gate: Arc<Gate>) -> Self {
        Self {
            files,
            next: 0,
            open: None,
            gate,
        }
    }

    fn open_next(&mut self) -> std::io::Result<bool> {
        let Some(path) = self.files.get(self.next) else { return Ok(false) };
        loop {
            match File::open(path) {
                Ok(f) => {
                    self.gate.done_waiting(path);
                    self.open = Some(f);
                    self.next += 1;
                    return Ok(true);
                }
                Err(_) if self.gate.failed.load(Relaxed) => return Err(std::io::Error::other("download failed")),
                Err(e) if self.gate.finished.load(Relaxed) => return Err(e),
                Err(_) => {
                    self.gate.wait_on(path);
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }
}

impl Read for StreamSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(f) = &mut self.open {
                let n = f.read(buf)?;
                if n > 0 || buf.is_empty() {
                    return Ok(n);
                }
                self.open = None;
            }
            if !self.open_next()? {
                return Ok(0);
            }
        }
    }
}

impl Seek for StreamSource {
    fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "forward-only stream"))
    }
}

impl MediaSource for StreamSource {
    fn is_seekable(&self) -> bool {
        false
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

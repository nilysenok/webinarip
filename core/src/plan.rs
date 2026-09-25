//! What to download for each track: the audio rendition's init piece and segments, each with
//! its place on the shared recording timeline, so a plan can be cut to any `[from, to)`.

use std::path::PathBuf;
use std::sync::Arc;

use crate::http::Http;
use crate::record::{Record, Track};
use crate::{Error, Result, cache, hls};

#[derive(Debug, Clone)]
pub struct Piece {
    /// Start on the recording timeline, seconds; `None` for the init piece.
    pub at: Option<f64>,
    pub end: f64,
    pub url: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Planned {
    pub track: Track,
    /// Position of the track in the download — the key for per-track progress.
    pub slot: usize,
    /// Init piece first, then segments in order.
    pub pieces: Vec<Piece>,
}

impl Planned {
    /// Only the segments overlapping `[from, to)` (the init piece is always kept).
    pub fn window(&self, from: f64, to: f64) -> Option<Planned> {
        let pieces: Vec<Piece> = self
            .pieces
            .iter()
            .filter(|p| p.at.is_none_or(|s| p.end > from && s < to))
            .cloned()
            .collect();
        pieces.iter().any(|p| p.at.is_some()).then(|| Planned { pieces, ..self.clone() })
    }

    /// Timeline start of the first segment.
    pub fn first(&self) -> f64 {
        self.pieces.iter().find_map(|p| p.at).unwrap_or(self.track.start)
    }

    pub fn files(&self) -> Vec<PathBuf> {
        self.pieces.iter().map(|p| p.path.clone()).collect()
    }
}

async fn plan_track(http: Arc<Http>, root: PathBuf, rec_id: String, track: Track, slot: usize) -> Result<Option<Planned>> {
    let Some(master_url) = track.hls.clone() else { return Ok(None) };
    let master = hls::parse_master(&master_url, &String::from_utf8_lossy(&http.get_ok(&master_url).await?))?;
    let Some(audio) = master.audio else { return Ok(None) };
    let media = hls::parse_media(&audio, &String::from_utf8_lossy(&http.get_ok(&audio).await?))?;
    let dir = cache::track_dir(&root, &rec_id, track.id, "a");
    let mut pieces = Vec::with_capacity(media.segments.len() + 1);
    if let Some(url) = media.init {
        pieces.push(Piece {
            at: None,
            end: track.start,
            url,
            path: cache::init_path(&dir),
        });
    }
    for (i, seg) in media.segments.iter().enumerate() {
        let at = track.start + seg.start;
        pieces.push(Piece {
            at: Some(at),
            end: at + seg.duration,
            url: seg.url.clone(),
            path: cache::segment_path(&dir, i),
        });
    }
    Ok(Some(Planned { track, slot, pieces }))
}

/// Plans the audio of `tracks`; tracks without an audio rendition are skipped.
pub async fn plan(http: &Arc<Http>, cache_dir: &std::path::Path, rec: &Record, tracks: Vec<Track>) -> Result<Vec<Planned>> {
    let mut set = tokio::task::JoinSet::new();
    for (slot, t) in tracks.into_iter().enumerate() {
        set.spawn(plan_track(http.clone(), cache_dir.to_path_buf(), rec.id.clone(), t, slot));
    }
    let mut planned = Vec::new();
    while let Some(r) = set.join_next().await {
        planned.extend(r.map_err(|e| Error::Net(e.to_string()))??);
    }
    planned.sort_by_key(|p| p.track.index);
    Ok(planned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(at: Option<f64>, end: f64) -> Piece {
        Piece {
            at,
            end,
            url: String::new(),
            path: PathBuf::from(format!("{at:?}")),
        }
    }

    #[test]
    fn window_keeps_init_and_overlapping_segments() {
        let track = Track {
            index: 1,
            id: 1,
            name: "a".into(),
            is_host: false,
            start: 10.0,
            duration: 30.0,
            hls: None,
        };
        let p = Planned {
            track,
            slot: 0,
            pieces: vec![
                piece(None, 10.0),
                piece(Some(10.0), 20.0),
                piece(Some(20.0), 30.0),
                piece(Some(30.0), 40.0),
            ],
        };
        let w = p.window(25.0, 32.0).unwrap();
        assert_eq!(
            w.pieces.iter().map(|p| p.at).collect::<Vec<_>>(),
            vec![None, Some(20.0), Some(30.0)]
        );
        assert_eq!(w.first(), 20.0);
        assert!(p.window(50.0, 60.0).is_none());
    }
}

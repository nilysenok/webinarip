//! What to download for each track: its audio rendition and, when asked, one video rendition —
//! init piece and segments, each with its place on the shared recording timeline, so a plan
//! can be cut to any `[from, to)`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::camera::has_video;
use crate::hls::{self, Media};
use crate::http::Http;
use crate::record::{Record, Track};
use crate::{Error, Result, cache};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Audio,
    /// Frame height of the chosen rendition, from the playlist.
    Video(u32),
}

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
    pub kind: Kind,
    /// Position in the download — the key for per-track progress.
    pub slot: usize,
    /// Init piece first, then segments in order.
    pub pieces: Vec<Piece>,
}

/// Which tracks get video, the highest frame height wanted (`None` = best) and the shortest
/// video worth keeping, measured on its playlist.
#[derive(Debug, Clone, Default)]
pub struct VideoPick {
    pub tracks: Vec<u64>,
    pub max_height: Option<u32>,
    pub min_secs: f64,
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

fn pieces(track: &Track, media: &Media, dir: &std::path::Path) -> Vec<Piece> {
    let mut out = Vec::with_capacity(media.segments.len() + 1);
    if let Some(url) = &media.init {
        out.push(Piece {
            at: None,
            end: track.start,
            url: url.clone(),
            path: cache::init_path(dir),
        });
    }
    for (i, seg) in media.segments.iter().enumerate() {
        let at = track.start + seg.start;
        out.push(Piece {
            at: Some(at),
            end: at + seg.duration,
            url: seg.url.clone(),
            path: cache::segment_path(dir, i),
        });
    }
    out
}

async fn plan_track(
    http: Arc<Http>,
    root: PathBuf,
    rec_id: String,
    track: Track,
    video: Option<(Option<u32>, f64)>,
) -> Result<Vec<Planned>> {
    let Some(master_url) = track.hls.clone() else { return Ok(vec![]) };
    let master = hls::parse_master(&master_url, &String::from_utf8_lossy(&http.get_ok(&master_url).await?))?;
    let mut out = Vec::new();
    if let Some(audio) = &master.audio {
        let media = hls::parse_media(audio, &String::from_utf8_lossy(&http.get_ok(audio).await?))?;
        let pieces = pieces(&track, &media, &cache::track_dir(&root, &rec_id, track.id, "a"));
        out.push(Planned {
            track: track.clone(),
            kind: Kind::Audio,
            slot: 0,
            pieces,
        });
    }
    let Some((v, min_secs)) = video.and_then(|(h, min)| Some((hls::pick(&master.variants, h)?, min))) else {
        return Ok(out);
    };
    let media = hls::parse_media(&v.url, &String::from_utf8_lossy(&http.get_ok(&v.url).await?))?;
    let dir = cache::track_dir(&root, &rec_id, track.id, &format!("v{}", v.height));
    // The length comes from the playlist: the recording's JSON has none for some sessions.
    let long = media.segments.iter().map(|s| s.duration).sum::<f64>() >= min_secs;
    if long && has_video(&http, &media, &dir).await? {
        out.push(Planned {
            kind: Kind::Video(v.height),
            slot: 0,
            pieces: pieces(&track, &media, &dir),
            track,
        });
    }
    Ok(out)
}

/// Plans the audio of `tracks` and the video of those in `video`. Tracks without the
/// rendition, without a camera or with a shorter video are skipped. Slots are numbered in
/// (track, kind) order.
pub async fn plan(http: &Arc<Http>, cache_dir: &Path, rec: &Record, tracks: Vec<Track>, video: Option<&VideoPick>) -> Result<Vec<Planned>> {
    let mut set = tokio::task::JoinSet::new();
    for t in tracks {
        let v = video.filter(|v| v.tracks.contains(&t.id)).map(|v| (v.max_height, v.min_secs));
        set.spawn(plan_track(http.clone(), cache_dir.to_path_buf(), rec.id.clone(), t, v));
    }
    let mut planned = Vec::new();
    while let Some(r) = set.join_next().await {
        planned.extend(r.map_err(|e| Error::Net(e.to_string()))??);
    }
    planned.sort_by_key(|p| (p.track.index, p.kind));
    planned.iter_mut().enumerate().for_each(|(i, p)| p.slot = i);
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
        let pieces = vec![
            piece(None, 10.0),
            piece(Some(10.0), 20.0),
            piece(Some(20.0), 30.0),
            piece(Some(30.0), 40.0),
        ];
        let p = Planned {
            track,
            kind: Kind::Audio,
            slot: 0,
            pieces,
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

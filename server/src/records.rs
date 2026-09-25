//! A recording as the interface sees it: tracks with per-track download progress.

use std::sync::atomic::Ordering::Relaxed;

use serde::Serialize;
use webinarip_core::engine::Download;
use webinarip_core::record::Record;

#[derive(Serialize)]
pub struct TrackView {
    pub index: usize,
    pub name: String,
    pub host: bool,
    pub start: f64,
    pub duration: f64,
    pub done: usize,
    pub total: usize,
}

#[derive(Serialize)]
pub struct RecordView {
    pub id: String,
    pub title: String,
    pub date: String,
    pub duration: f64,
    pub tracks: Vec<TrackView>,
    /// Segments of the whole recording already on disk (speculative download).
    pub ready: usize,
    pub planned: usize,
}

pub fn record_view(rec: &Record, dl: Option<&Download>) -> RecordView {
    let slots = dl.map(|d| d.prog.slots()).unwrap_or_default();
    let tracks = rec
        .tracks
        .iter()
        .map(|t| {
            let (done, total) = dl
                .and_then(|d| d.slot_of(t.id))
                .and_then(|s| slots.get(s).copied())
                .unwrap_or((0, 0));
            TrackView {
                index: t.index,
                name: t.name.clone(),
                host: t.is_host,
                start: t.start,
                duration: t.duration,
                done,
                total,
            }
        })
        .collect();
    let (ready, planned) = dl.map_or((0, 0), |d| (d.prog.seg_done.load(Relaxed), d.prog.seg_total.load(Relaxed)));
    RecordView {
        id: rec.id.clone(),
        title: rec.title.clone(),
        date: rec.date.clone(),
        duration: rec.duration,
        tracks,
        ready,
        planned,
    }
}

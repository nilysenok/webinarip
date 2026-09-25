//! WebM writer: VP9 frames and Opus packets go in as they are, no re-encoding. Clusters start
//! on key frames, a Cues index makes the file seekable, and the segment size, duration and
//! SeekHead are patched in at the end.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use super::ebml::{element, float, id, master, string, uint, vint_size, vint8};
use crate::Result;

const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;
const SEEK_RESERVE: usize = 96;
const CLUSTER_MS: i64 = 5_000;

pub const VIDEO: u8 = 1;
pub const AUDIO: u8 = 2;

pub struct Audio {
    pub channels: u8,
    /// `OpusHead` — the CodecPrivate of an Opus track.
    pub head: Vec<u8>,
    pub delay_ns: u64,
}

pub struct WebmWriter {
    out: BufWriter<File>,
    segment_size_at: u64,
    data_start: u64,
    duration_at: u64,
    info_at: u64,
    tracks_at: u64,
    cluster: Vec<u8>,
    cluster_ts: i64,
    cues: Vec<(i64, u64)>,
}

fn track_entry(number: u8, kind: u8, codec: &str, extra: Vec<Vec<u8>>) -> Vec<u8> {
    let mut children = vec![
        uint(0xD7, number as u64),
        uint(0x73C5, number as u64),
        uint(0x83, kind as u64),
        uint(0x9C, 0),
        string(0x86, codec),
    ];
    children.extend(extra);
    master(0xAE, &children)
}

impl WebmWriter {
    pub fn create(path: &Path, (width, height): (u16, u16), audio: Option<&Audio>) -> Result<Self> {
        let mut out = BufWriter::new(File::create(path)?);
        let ebml = [
            uint(0x4286, 1),
            uint(0x42F7, 1),
            uint(0x42F2, 4),
            uint(0x42F3, 8),
            string(0x4282, "webm"),
            uint(0x4287, 4),
            uint(0x4285, 2),
        ];
        out.write_all(&master(0x1A45_DFA3, &ebml))?;
        out.write_all(&id(SEGMENT))?;
        let segment_size_at = out.stream_position()?;
        out.write_all(&vint8(0))?;
        let data_start = out.stream_position()?;
        out.write_all(&element(0xEC, &[0; SEEK_RESERVE - 2]))?; // Void, becomes the SeekHead
        let info_at = out.stream_position()? - data_start;
        let info = master(
            INFO,
            &[
                uint(0x2AD7B1, 1_000_000),
                string(0x4D80, "webinarip"),
                string(0x5741, "webinarip"),
                float(0x4489, 0.0),
            ],
        );
        let duration_at = out.stream_position()? + info.len() as u64 - 8;
        out.write_all(&info)?;
        let tracks_at = out.stream_position()? - data_start;
        let mut entries = vec![track_entry(
            VIDEO,
            1,
            "V_VP9",
            vec![master(0xE0, &[uint(0xB0, width as u64), uint(0xBA, height as u64)])],
        )];
        if let Some(a) = audio {
            let audio_el = master(0xE1, &[float(0xB5, 48_000.0), uint(0x9F, a.channels as u64)]);
            entries.push(track_entry(
                AUDIO,
                2,
                "A_OPUS",
                vec![
                    element(0x63A2, &a.head),
                    uint(0x56AA, a.delay_ns),
                    uint(0x56BB, 80_000_000),
                    audio_el,
                ],
            ));
        }
        out.write_all(&master(TRACKS, &entries))?;
        Ok(Self {
            out,
            segment_size_at,
            data_start,
            duration_at,
            info_at,
            tracks_at,
            cluster: Vec::new(),
            cluster_ts: 0,
            cues: Vec::new(),
        })
    }

    fn flush_cluster(&mut self) -> Result<()> {
        if self.cluster.is_empty() {
            return Ok(());
        }
        let mut body = uint(0xE7, self.cluster_ts.max(0) as u64);
        body.append(&mut self.cluster);
        self.out.write_all(&element(CLUSTER, &body))?;
        Ok(())
    }

    /// Adds one frame or packet; calls must come in timestamp order.
    pub fn block(&mut self, track: u8, ts_ms: i64, key: bool, data: &[u8]) -> Result<()> {
        let rel = ts_ms - self.cluster_ts;
        let new_cluster =
            self.cluster.is_empty() || (track == VIDEO && key && rel >= CLUSTER_MS) || !(i16::MIN as i64..=i16::MAX as i64).contains(&rel);
        if new_cluster {
            self.flush_cluster()?;
            self.cluster_ts = ts_ms;
            if track == VIDEO && key {
                let at = self.out.stream_position()? - self.data_start;
                self.cues.push((ts_ms, at));
            }
        }
        let rel = (ts_ms - self.cluster_ts) as i16;
        let mut b = vec![0x80 | track];
        b.extend(rel.to_be_bytes());
        b.push(if key { 0x80 } else { 0 });
        b.extend_from_slice(data);
        self.cluster.extend(element(0xA3, &b));
        Ok(())
    }

    pub fn finish(mut self, duration_ms: f64) -> Result<()> {
        self.flush_cluster()?;
        let cues_at = self.out.stream_position()? - self.data_start;
        let points: Vec<Vec<u8>> = self
            .cues
            .iter()
            .map(|&(t, pos)| {
                master(
                    0xBB,
                    &[uint(0xB3, t.max(0) as u64), master(0xB7, &[uint(0xF7, 1), uint(0xF1, pos)])],
                )
            })
            .collect();
        self.out.write_all(&master(CUES, &points))?;
        let end = self.out.stream_position()?;
        let seek = |el: u32, pos: u64| master(0x4DBB, &[element(0x53AB, &id(el)), uint(0x53AC, pos)]);
        let head = master(
            0x114D_9B74,
            &[seek(INFO, self.info_at), seek(TRACKS, self.tracks_at), seek(CUES, cues_at)],
        );
        let pad = SEEK_RESERVE - head.len();
        let mut void = id(0xEC);
        void.extend(vint_size((pad - 2) as u64));
        void.resize(pad, 0);
        self.out.seek(SeekFrom::Start(self.data_start))?;
        self.out.write_all(&head)?;
        self.out.write_all(&void)?;
        self.out.seek(SeekFrom::Start(self.duration_at))?;
        self.out.write_all(&duration_ms.to_be_bytes())?;
        self.out.seek(SeekFrom::Start(self.segment_size_at))?;
        self.out.write_all(&vint8(end - self.data_start))?;
        self.out.flush()?;
        Ok(())
    }
}

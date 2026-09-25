//! Multicam timeline for editors: FCPXML (Final Cut Pro, DaVinci Resolve) with every video on
//! its own lane and the audio below, plus a CMX3600 EDL. Positions snap to 25 fps frames.

use std::fmt::Write;
use std::path::Path;

use crate::deliver::{Made, Piece};

const FPS: f64 = 25.0;

fn frames(secs: f64) -> i64 {
    (secs * FPS).round() as i64
}

fn t(secs: f64) -> String {
    format!("{}/25s", frames(secs))
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `file://` URL with everything outside the unreserved set percent-encoded.
pub fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(b as char),
            _ => write!(out, "%{b:02X}").unwrap(),
        }
    }
    out
}

pub fn fcpxml(title: &str, total: f64, pieces: &[Piece]) -> String {
    let (w, h) = pieces
        .iter()
        .find_map(|p| {
            if let Made::Video { width, height } = p.kind {
                Some((width, height))
            } else {
                None
            }
        })
        .unwrap_or((1280, 720));
    let mut res = format!("    <format id=\"r0\" frameDuration=\"1/25s\" width=\"{w}\" height=\"{h}\"/>\n");
    let (mut clips, mut video_lane, mut audio_lane) = (String::new(), 0, 0);
    for (i, p) in pieces.iter().enumerate() {
        let video = matches!(p.kind, Made::Video { .. });
        let lane = if video {
            video_lane += 1;
            video_lane
        } else {
            audio_lane -= 1;
            audio_lane
        };
        let (name, dur) = (xml(&p.name), t(p.duration));
        writeln!(
            res,
            "    <asset id=\"a{i}\" name=\"{name}\" start=\"0s\" duration=\"{dur}\" hasVideo=\"{}\" hasAudio=\"1\" format=\"r0\" audioSources=\"1\" audioRate=\"48000\">\n      <media-rep kind=\"original-media\" src=\"{}\"/>\n    </asset>",
            u8::from(video),
            file_url(&p.path)
        )
        .unwrap();
        // A clip that begins before the range sits at 0 with its head skipped.
        let (skip, offset) = ((-p.offset).max(0.0), p.offset.max(0.0));
        let length = t(p.duration - skip);
        writeln!(
            clips,
            "            <asset-clip ref=\"a{i}\" lane=\"{lane}\" offset=\"{}\" name=\"{name}\" duration=\"{length}\" start=\"{}\"/>",
            t(offset),
            t(skip)
        )
        .unwrap();
    }
    let (title, total) = (xml(title), t(total));
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.10\">\n  <resources>\n{res}  </resources>\n  <library>\n    <event name=\"{title}\">\n      <project name=\"{title}\">\n        <sequence format=\"r0\" duration=\"{total}\" tcStart=\"0s\" tcFormat=\"NDF\" audioLayout=\"stereo\" audioRate=\"48k\">\n          <spine>\n            <gap name=\"timeline\" offset=\"0s\" duration=\"{total}\" start=\"0s\">\n{clips}            </gap>\n          </spine>\n        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n"
    )
}

fn tc(secs: f64) -> String {
    let f = frames(secs).max(0);
    let s = f / 25;
    format!("{:02}:{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60, f % 25)
}

/// CMX3600: one event per file, sorted by position. EDL is a single-track format by design —
/// the full multicam layout is in the FCPXML.
pub fn edl(title: &str, pieces: &[Piece]) -> String {
    let mut sorted: Vec<&Piece> = pieces.iter().collect();
    sorted.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    let mut out = format!("TITLE: {title}\nFCM: NON-DROP FRAME\n\n");
    for (i, p) in sorted.iter().enumerate() {
        let track = if matches!(p.kind, Made::Video { .. }) { "B    " } else { "A    " };
        let skip = (-p.offset).max(0.0);
        let (src_in, src_out, rec_in, rec_out) = (tc(skip), tc(p.duration), tc(p.offset.max(0.0)), tc(p.offset + p.duration));
        let file = p.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        writeln!(
            out,
            "{:03}  AX       {track} C        {src_in} {src_out} {rec_in} {rec_out}\n* FROM CLIP NAME: {file}\n",
            i + 1
        )
        .unwrap();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pieces() -> Vec<Piece> {
        vec![
            Piece {
                kind: Made::Mix,
                path: PathBuf::from("/o/mix.mp3"),
                name: "Mix".into(),
                offset: 0.0,
                duration: 100.0,
            },
            Piece {
                kind: Made::Video { width: 640, height: 360 },
                path: PathBuf::from("/o/video/03 Анна 0-02-15.webm"),
                name: "Анна & Co".into(),
                offset: 135.04,
                duration: 60.0,
            },
        ]
    }

    #[test]
    fn fcpxml_lanes_offsets_and_urls() {
        let x = fcpxml("Вебинар", 200.0, &pieces());
        assert!(x.contains("lane=\"-1\" offset=\"0/25s\" name=\"Mix\""));
        assert!(x.contains("lane=\"1\" offset=\"3376/25s\" name=\"Анна &amp; Co\""));
        assert!(x.contains("file:///o/video/03%20%D0%90%D0%BD%D0%BD%D0%B0%200-02-15.webm"));
        assert!(x.contains("width=\"640\" height=\"360\""));
        let early = [Piece {
            kind: Made::Track,
            path: PathBuf::from("/t.m4a"),
            name: "T".into(),
            offset: -2.0,
            duration: 10.0,
        }];
        assert!(fcpxml("W", 20.0, &early).contains("offset=\"0/25s\" name=\"T\" duration=\"200/25s\" start=\"50/25s\""));
        assert!(edl("W", &early).contains("00:00:02:00 00:00:10:00 00:00:00:00 00:00:08:00"));
    }

    #[test]
    fn edl_events_are_sorted_with_timecodes() {
        let e = edl("Webinar", &pieces());
        assert!(e.starts_with("TITLE: Webinar\nFCM: NON-DROP FRAME"));
        assert!(e.contains("001  AX       A     C        00:00:00:00 00:01:40:00 00:00:00:00 00:01:40:00"));
        assert!(e.contains("002  AX       B     C        00:00:00:00 00:01:00:00 00:02:15:01 00:03:15:01"));
    }
}

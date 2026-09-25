//! Video end to end on the mock server: WebM per participant (VP9 copied + mixed Opus),
//! participants' own tracks, and the multicam timeline.

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use webinarip_core::deliver::Made;
use webinarip_core::encode::{Format, Quality};
use webinarip_core::job::{self, Options, What};
use webinarip_core::progress::Progress;

fn ffprobe(path: &std::path::Path, entries: &str) -> Option<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", entries, "-of", "csv=p=0"])
        .arg(path)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn videos_tracks_and_timeline() {
    let cfg = common::Config {
        per_conn_rate: 1_000_000,
        first_429: 0,
        private: false,
        stall: None,
    };
    let mock = common::start(cfg).await;
    let dir = std::env::temp_dir().join(format!("webinarip-video-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut o = Options::new(format!("http://{}/j/1/2/record-new/1", mock.addr));
    (o.api_base, o.what, o.format, o.quality) = (format!("http://{}", mock.addr), What::Both, Format::Wav, Quality::Speech);
    (o.out_dir, o.cache_dir, o.connections, o.multicam, o.tracks) = (dir.join("out"), dir.join("cache"), 8, true, Some("1,2".into()));
    let out = job::run(o, Arc::new(Progress::default()), |_| {}).await.unwrap();

    let videos: Vec<_> = out.pieces.iter().filter(|p| matches!(p.kind, Made::Video { .. })).collect();
    let tracks: Vec<_> = out.pieces.iter().filter(|p| p.kind == Made::Track).collect();
    assert_eq!((videos.len(), tracks.len()), (2, 2));
    // Real frame size from the track header, not the playlist's 640x480.
    assert_eq!(videos[0].kind, Made::Video { width: 160, height: 90 });
    assert!(
        (videos[1].offset - 2.0).abs() < 0.05,
        "second video starts at 2 s: {}",
        videos[1].offset
    );
    for v in &videos {
        let bytes = std::fs::read(&v.path).unwrap();
        assert_eq!(&bytes[..4], &[0x1a, 0x45, 0xdf, 0xa3], "EBML header");
        let has = |s: &[u8]| bytes.windows(s.len()).any(|w| w == s);
        assert!(has(b"V_VP9") && has(b"A_OPUS") && has(b"webinarip"));
        assert!((v.duration - 3.0).abs() < 0.2, "{}", v.duration);
        if let Some(streams) = ffprobe(&v.path, "stream=codec_name") {
            assert_eq!(streams.lines().collect::<Vec<_>>(), vec!["vp9", "opus"]);
        }
    }
    if let Some(d) = ffprobe(&tracks[0].path, "format=duration") {
        assert!((d.parse::<f64>().unwrap() - 3.0).abs() < 0.1, "m4a duration {d}");
    }
    let fcp = std::fs::read_to_string(out.dir.join("Mock webinar.fcpxml")).unwrap();
    assert!(fcp.contains("lane=\"2\"") && fcp.contains("lane=\"-3\""), "{fcp}");
    assert!(
        std::fs::read_to_string(out.dir.join("Mock webinar.edl"))
            .unwrap()
            .contains("FROM CLIP NAME: 02 Guest")
    );
    tokio::time::sleep(Duration::from_millis(10)).await;
    std::fs::remove_dir_all(dir).unwrap();
}

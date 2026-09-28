//! Camera-off sessions: MTS Link lists a 640x480 video rendition for them too, with only AAC
//! behind it. They must not turn into videos; a camera whose session has no length in the
//! recording's JSON must not be dropped.

mod common;

use std::sync::Arc;

use webinarip_core::deliver::Made;
use webinarip_core::encode::{Format, Quality};
use webinarip_core::engine::Engine;
use webinarip_core::job::{self, Options, What};
use webinarip_core::plan::VideoPick;
use webinarip_core::progress::Progress;

fn mock_config() -> common::Config {
    common::Config {
        per_conn_rate: 1_000_000,
        first_429: 0,
        private: false,
        stall: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn camera_off_track_gets_no_video() {
    let mock = common::start(mock_config()).await;
    let dir = std::env::temp_dir().join(format!("webinarip-nocam-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut o = Options::new(format!("http://{}/j/1/2/record-new/3", mock.addr));
    (o.api_base, o.what, o.format, o.quality) = (format!("http://{}", mock.addr), What::Video, Format::Wav, Quality::Speech);
    (o.out_dir, o.cache_dir, o.connections, o.tracks) = (dir.join("out"), dir.join("cache"), 8, Some("1,2,3".into()));
    let out = job::run(o, Arc::new(Progress::default()), |_| {}).await.unwrap();

    let videos: Vec<_> = out.pieces.iter().filter(|p| matches!(p.kind, Made::Video { .. })).collect();
    let names: Vec<_> = videos.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["Host", "Guest"], "no video for the camera-off track");
    for v in &videos {
        let bytes = std::fs::read(&v.path).unwrap();
        assert!(bytes.windows(5).any(|w| w == b"V_VP9"), "{}", v.path.display());
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn video_length_comes_from_the_playlist() {
    let mock = common::start(mock_config()).await;
    let dir = std::env::temp_dir().join(format!("webinarip-len-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let base = format!("http://{}", mock.addr);
    let engine = Engine::new(None, 8, &base, dir.clone()).unwrap();
    let rec = engine.record(&format!("{base}/j/1/2/record-new/3")).await.unwrap();
    let guest = rec.tracks.iter().find(|t| t.name == "Guest").unwrap();
    assert_eq!(guest.duration, 0.0, "the JSON has no length for the guest");

    // Without --tracks every track is a candidate; the length is judged when planning.
    let mut o = Options::new(String::new());
    o.what = What::Video;
    let pick = job::video_pick(&o, &rec.tracks).unwrap();
    assert!(pick.tracks.contains(&guest.id));

    let ids: Vec<u64> = rec.tracks.iter().map(|t| t.id).collect();
    for (min_secs, want) in [(2.5, vec!["Host", "Guest"]), (3.5, vec![])] {
        let pick = VideoPick { min_secs, ..pick.clone() };
        let dl = engine
            .download(&rec, rec.tracks.clone(), (0.0, rec.duration), Some(&pick))
            .await
            .unwrap();
        let got: Vec<_> = dl
            .of_kind(0.0, rec.duration, &ids, true)
            .iter()
            .map(|p| p.track.name.clone())
            .collect();
        assert_eq!(got, want, "min {min_secs} s");
        dl.wait().await.unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}

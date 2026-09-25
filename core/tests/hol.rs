//! The mixer is never blocked for long by one slow segment (no head-of-line blocking).

mod common;

#[tokio::test(flavor = "multi_thread")]
async fn a_stuck_segment_does_not_stall_the_mixer() {
    let (worst, p50, total) = common::hol::stalled_run("rush").await;
    println!("longest mixer stall {worst:?}, median segment {p50:?}, total {total:?}");
    assert!(worst <= p50 * 2, "mixer stalled {worst:?} > 2 × median {p50:?}");
    assert!(total.as_secs_f64() < 5.0, "the 10 s stall leaked into the total: {total:?}");
}

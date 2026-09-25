//! Baseline for the head-of-line test: the same run with rushing switched off.
//! `cargo test --release --test hol_baseline -- --ignored --nocapture`

mod common;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "diagnostic: shows the stall without rushing"]
async fn baseline_without_rush() {
    // SAFETY: this test binary runs only this test, so nothing reads the environment concurrently.
    unsafe { std::env::set_var("WEBINARIP_NO_RUSH", "1") };
    let (worst, p50, total) = common::hol::stalled_run("baseline").await;
    println!("WITHOUT rush: longest mixer stall {worst:?}, median segment {p50:?}, total {total:?}");
}

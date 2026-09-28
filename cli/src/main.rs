//! `webinarip <link>` — download a webinar recording: audio, videos, participants' tracks.
//! Only for recordings you have the rights to.

mod args;
mod bars;
mod serve;

use std::sync::Arc;
use std::time::Instant;

use args::{Cli, options};
use clap::Parser;
use webinarip_core::deliver::Made;
use webinarip_core::job::{self, Options};
use webinarip_core::progress::Progress;
use webinarip_core::{Error, http::Http, timefmt};

async fn list(opts: &Options) -> Result<(), Error> {
    let http = Http::new(opts.session_id.as_deref(), 4)?;
    let rec = job::fetch_record(&http, opts).await?;
    println!("{}  ·  {}  ·  {}", rec.title, rec.date, timefmt::hms(rec.duration));
    for t in &rec.tracks {
        let host = if t.is_host { " (host)" } else { "" };
        println!(
            "{:>3}  {:>8} – {:<8} {}{host}",
            t.index,
            timefmt::hms(t.start),
            timefmt::hms(t.start + t.duration),
            t.name
        );
    }
    Ok(())
}

async fn run(cli: Cli) -> Result<(), Error> {
    let opts = options(&cli)?;
    if opts.mp4 {
        eprintln!("note: --mp4 re-encodes every video with ffmpeg — expect minutes, not seconds");
    }
    if cli.list {
        return list(&opts).await;
    }
    let wants_video = opts.wants_video();
    let (prog, download) = (Arc::new(Progress::default()), Arc::new(std::sync::OnceLock::new()));
    let t0 = Instant::now();
    let drawer = tokio::spawn(bars::draw(prog.clone(), download.clone()));
    let result = job::run(opts, prog, |d| {
        let _ = download.set(d);
    })
    .await;
    drawer.abort();
    let out = result?;
    bars::summary(&out, t0.elapsed());
    if wants_video && !out.pieces.iter().any(|p| matches!(p.kind, Made::Video { .. })) {
        eprintln!("note: no videos — the chosen tracks have no camera (or, without --tracks, run under 30 s)");
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("serve") {
        let rest = std::iter::once(args[0].clone()).chain(args[2..].iter().cloned()).collect();
        if let Err(e) = serve::run(rest).await {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

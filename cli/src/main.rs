//! `webinarip <link>` — download a webinar recording as one audio file.
//! Only for recordings you have the rights to.

mod bars;
mod serve;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use clap::{ArgGroup, Parser, ValueEnum};
use webinarip_core::encode::{Format, Quality};
use webinarip_core::job::{self, Options};
use webinarip_core::progress::Progress;
use webinarip_core::{Error, HARD_CAP, http::Http, timefmt};

#[derive(Copy, Clone, ValueEnum)]
enum Q {
    /// 48 kHz stereo
    High,
    /// 16 kHz mono, small — speech and transcription
    Low,
}

#[derive(Parser)]
#[command(
    version,
    about = "Download a webinar recording as one audio file. Only for recordings you have the rights to."
)]
#[command(group(ArgGroup::new("what").args(["audio", "video", "both"])))]
#[command(group(ArgGroup::new("codec").args(["opus", "aac", "wav"])))]
struct Cli {
    /// Recording link: https://…/record-new/<id>
    link: String,
    /// Audio only (default)
    #[arg(long)]
    audio: bool,
    /// Video (not in this version yet)
    #[arg(long)]
    video: bool,
    /// Audio and video (not in this version yet)
    #[arg(long)]
    both: bool,
    /// Start of the range: 75, 01:15, 1:02:03, 1h2m
    #[arg(long, value_name = "TIME")]
    from: Option<String>,
    /// End of the range
    #[arg(long, value_name = "TIME")]
    to: Option<String>,
    /// Tracks to mix: host,3,5 (numbers from --list); all by default
    #[arg(long, value_name = "LIST")]
    tracks: Option<String>,
    #[arg(long, value_enum, default_value = "high")]
    quality: Q,
    /// Opus in Ogg instead of MP3 (smallest files)
    #[arg(long)]
    opus: bool,
    /// AAC in .m4a (AudioToolbox on macOS, ffmpeg elsewhere)
    #[arg(long)]
    aac: bool,
    /// 16-bit WAV (for transcription tools)
    #[arg(long)]
    wav: bool,
    /// Output directory; every run gets its own dated folder inside
    #[arg(long, default_value = ".")]
    out: PathBuf,
    /// Parallel connections, at most 256
    #[arg(long, default_value_t = HARD_CAP, value_name = "N")]
    connections: usize,
    /// Session id for private recordings (never written to disk)
    #[arg(long, env = "WEBINARIP_SESSION_ID", hide_env_values = true)]
    session_id: Option<String>,
    /// Segment cache directory
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// List the tracks of the recording and exit
    #[arg(long)]
    list: bool,
    /// Metadata API base URL
    #[arg(long, hide = true)]
    api: Option<String>,
}

fn options(cli: &Cli) -> Result<Options, Error> {
    let mut o = Options::new(&cli.link);
    o.session_id = cli.session_id.clone();
    o.format = if cli.opus {
        Format::Opus
    } else if cli.aac {
        Format::Aac
    } else if cli.wav {
        Format::Wav
    } else {
        Format::Mp3
    };
    o.quality = match cli.quality {
        Q::High => Quality::High,
        Q::Low => Quality::Low,
    };
    o.from = cli.from.as_deref().map(timefmt::parse).transpose()?;
    o.to = cli.to.as_deref().map(timefmt::parse).transpose()?;
    o.tracks = cli.tracks.clone();
    o.out_dir = cli.out.clone();
    o.connections = cli.connections.clamp(1, HARD_CAP);
    if let Some(d) = &cli.cache_dir {
        o.cache_dir = d.clone();
    }
    if let Some(a) = &cli.api {
        o.api_base = a.clone();
    }
    Ok(o)
}

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
    if cli.video || cli.both {
        return Err(Error::Usage("video is coming in the next version; for now use --audio".into()));
    }
    let opts = options(&cli)?;
    if cli.list {
        return list(&opts).await;
    }
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

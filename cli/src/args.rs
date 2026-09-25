//! Command-line arguments and how they map onto [`Options`].

use std::path::PathBuf;

use clap::{ArgGroup, Parser, ValueEnum};
use webinarip_core::encode::{Format, Quality};
use webinarip_core::job::{Options, What};
use webinarip_core::{Error, HARD_CAP, timefmt};

#[derive(Copy, Clone, ValueEnum)]
pub enum Q {
    /// Speech (default): mono, MP3 64 kbit/s — half the size of stereo, same voice
    Speech,
    /// Stereo 48 kHz, MP3 128 kbit/s
    High,
    /// 16 kHz mono, lowest bitrate — for transcription
    Low,
}

#[derive(Parser)]
#[command(
    version,
    about = "Download a webinar recording: the mixed audio, participants' videos, their own tracks. Only for recordings you have the rights to.",
    after_help = "Web interface: webinarip serve [--port N] [--out DIR]"
)]
#[command(group(ArgGroup::new("what").args(["audio", "video", "both"])))]
#[command(group(ArgGroup::new("codec").args(["opus", "aac", "wav"])))]
pub struct Cli {
    /// Recording link: https://…/record-new/<id>
    pub link: String,
    /// The mixed audio file (default)
    #[arg(long)]
    pub audio: bool,
    /// Participants' videos: WebM (VP9 copied as is + the mixed audio in Opus)
    #[arg(long)]
    pub video: bool,
    /// The mixed audio file and the videos
    #[arg(long)]
    pub both: bool,
    /// Start of the range: 75, 01:15, 1:02:03, 1h2m
    #[arg(long, value_name = "TIME")]
    pub from: Option<String>,
    /// End of the range
    #[arg(long, value_name = "TIME")]
    pub to: Option<String>,
    /// Tracks: host,3,5 (numbers from --list); all by default (videos: those over 30 s)
    #[arg(long, value_name = "LIST")]
    pub tracks: Option<String>,
    #[arg(long, value_enum, default_value = "speech")]
    pub quality: Q,
    /// Opus in Ogg instead of MP3 (smallest files)
    #[arg(long)]
    pub opus: bool,
    /// AAC in .m4a (AudioToolbox on macOS, ffmpeg elsewhere)
    #[arg(long)]
    pub aac: bool,
    /// 16-bit WAV (for transcription tools)
    #[arg(long)]
    pub wav: bool,
    /// Highest video frame height, e.g. 480; best available by default
    #[arg(long, value_name = "PIXELS")]
    pub video_height: Option<u32>,
    /// Also each participant's own audio in original quality (.m4a, not re-encoded)
    #[arg(long)]
    pub separate: bool,
    /// Also a multicam timeline for editors: FCPXML + EDL (includes --separate)
    #[arg(long)]
    pub multicam: bool,
    /// Videos as H.264/AAC MP4 instead of WebM — re-encodes with the system ffmpeg, slow
    #[arg(long)]
    pub mp4: bool,
    /// Output directory; every run gets its own dated folder inside
    #[arg(long, default_value = ".")]
    pub out: PathBuf,
    /// Parallel connections, at most 256
    #[arg(long, default_value_t = HARD_CAP, value_name = "N")]
    pub connections: usize,
    /// Session id for private recordings (never written to disk)
    #[arg(long, env = "WEBINARIP_SESSION_ID", hide_env_values = true)]
    pub session_id: Option<String>,
    /// Segment cache directory
    #[arg(long, value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
    /// List the tracks of the recording and exit
    #[arg(long)]
    pub list: bool,
    /// Metadata API base URL
    #[arg(long, hide = true)]
    pub api: Option<String>,
}

pub fn options(cli: &Cli) -> Result<Options, Error> {
    let mut o = Options::new(&cli.link);
    o.session_id = cli.session_id.clone();
    o.what = if cli.video {
        What::Video
    } else if cli.both {
        What::Both
    } else {
        What::Audio
    };
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
        Q::Speech => Quality::Speech,
        Q::High => Quality::High,
        Q::Low => Quality::Low,
    };
    o.from = cli.from.as_deref().map(timefmt::parse).transpose()?;
    o.to = cli.to.as_deref().map(timefmt::parse).transpose()?;
    o.tracks = cli.tracks.clone();
    o.video_height = cli.video_height;
    (o.separate, o.multicam, o.mp4) = (cli.separate, cli.multicam, cli.mp4);
    if cli.mp4 && o.what == What::Audio {
        return Err(Error::Usage("--mp4 is about videos: add --video or --both".into()));
    }
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

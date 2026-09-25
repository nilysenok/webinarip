//! `webinarip serve`: argument parsing for the local web interface.

use std::path::PathBuf;

use clap::Parser;
use webinarip_core::Error;

/// `webinarip serve` — the local web interface.
#[derive(Parser)]
#[command(name = "webinarip serve", bin_name = "webinarip serve", about = "Open the local web interface (127.0.0.1 only)")]
struct Serve {
    /// Port; a free one is picked if it is busy
    #[arg(long, default_value_t = 5040)]
    port: u16,
    /// Where results go; every run gets its own dated folder inside
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// Do not open the browser
    #[arg(long)]
    no_open: bool,
    /// Segment cache directory
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    #[arg(long, hide = true)]
    api: Option<String>,
}

pub async fn run(args: Vec<String>) -> Result<(), Error> {
    let s = Serve::parse_from(args);
    let out_dir = s.out.unwrap_or_else(|| dirs_downloads().join("webinarip"));
    webinarip_server::serve(webinarip_server::ServeOptions {
        port: s.port,
        out_dir,
        cache_dir: s.cache_dir.unwrap_or_else(webinarip_core::cache::default_root),
        api_base: s.api.unwrap_or_else(|| webinarip_core::record::DEFAULT_API.into()),
        open_browser: !s.no_open,
    })
    .await
    .map_err(Error::Io)
}

fn dirs_downloads() -> PathBuf {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Downloads"))
        .unwrap_or_else(|| PathBuf::from("."))
}

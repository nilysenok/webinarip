//! Opening the browser and the file manager with the platform's own tools.

use std::path::Path;
use std::process::{Command, Stdio};

fn spawn(cmd: &str, args: &[&str]) {
    let _ = Command::new(cmd).args(args).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
}

pub fn browser(url: &str) {
    if cfg!(target_os = "macos") {
        spawn("open", &[url]);
    } else if cfg!(windows) {
        spawn("cmd", &["/C", "start", "", url]);
    } else {
        spawn("xdg-open", &[url]);
    }
}

/// Selects the file in Finder / Explorer; on Linux opens its folder.
pub fn reveal(path: &Path) {
    let p = path.display().to_string();
    if cfg!(target_os = "macos") {
        spawn("open", &["-R", &p]);
    } else if cfg!(windows) {
        spawn("explorer", &[&format!("/select,{p}")]);
    } else if let Some(dir) = path.parent() {
        spawn("xdg-open", &[&dir.display().to_string()]);
    }
}

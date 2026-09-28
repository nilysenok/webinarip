//! Whether a "video" rendition really carries video. MTS Link gives camera-off sessions a
//! video rendition too, labelled 640x480 in the master playlist, with only AAC behind it.

use std::path::Path;

use crate::hls::Media;
use crate::http::Http;
use crate::video::fmp4;
use crate::{Result, cache};

/// The init piece decides (`hdlr` = `vide`). It goes to the cache, so the download that
/// follows does not fetch it again.
pub(crate) async fn has_video(http: &Http, media: &Media, dir: &Path) -> Result<bool> {
    let Some(url) = &media.init else { return Ok(false) };
    let path = cache::init_path(dir);
    let init = match std::fs::read(&path) {
        Ok(init) => init,
        Err(_) => {
            let init = http.get_ok(url).await?.to_vec();
            cache::write_atomic(&path, &init)?;
            init
        }
    };
    Ok(fmp4::is_video(&init))
}

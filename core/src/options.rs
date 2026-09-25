//! What to download and how.

use std::path::PathBuf;

use crate::encode::{Format, Quality};
use crate::{cache, record};

#[derive(Debug, Clone)]
pub struct Options {
    pub link: String,
    /// For private recordings. Kept in memory only.
    pub session_id: Option<String>,
    pub api_base: String,
    pub format: Format,
    pub quality: Quality,
    pub from: Option<f64>,
    pub to: Option<f64>,
    pub tracks: Option<String>,
    pub out_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub connections: usize,
}

impl Options {
    pub fn new(link: impl Into<String>) -> Self {
        Self {
            link: link.into(),
            session_id: None,
            api_base: record::DEFAULT_API.into(),
            format: Format::Mp3,
            quality: Quality::Speech,
            from: None,
            to: None,
            tracks: None,
            out_dir: PathBuf::from("."),
            cache_dir: cache::default_root(),
            connections: crate::HARD_CAP,
        }
    }
}

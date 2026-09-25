//! HLS playlists: master (audio rendition + video variants) and media (init + segments).

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub width: u32,
    pub height: u32,
    pub bandwidth: u64,
    pub url: String,
}

#[derive(Debug, Clone, Default)]
pub struct Master {
    pub audio: Option<String>,
    /// Best first.
    pub variants: Vec<Variant>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub url: String,
    /// Start inside the track, seconds (sum of previous EXTINF).
    pub start: f64,
    pub duration: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Media {
    pub init: Option<String>,
    pub segments: Vec<Segment>,
}

/// Best variant, or the highest one not above `max_height` (the smallest if none fits).
pub fn pick(variants: &[Variant], max_height: Option<u32>) -> Option<&Variant> {
    match max_height {
        None => variants.first(),
        Some(h) => variants.iter().find(|v| v.height <= h).or(variants.last()),
    }
}

pub fn join(base: &str, rel: &str) -> Result<String> {
    let base = url::Url::parse(base).map_err(|e| Error::Parse(format!("{base}: {e}")))?;
    Ok(base.join(rel).map_err(|e| Error::Parse(format!("{rel}: {e}")))?.to_string())
}

fn attr<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("{key}="))? + key.len() + 1;
    let rest = &line[at..];
    if let Some(stripped) = rest.strip_prefix('"') {
        stripped.split('"').next()
    } else {
        rest.split(',').next()
    }
}

pub fn parse_master(base: &str, text: &str) -> Result<Master> {
    let mut m = Master::default();
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-MEDIA:") && attr(line, "TYPE") == Some("AUDIO") {
            if let Some(uri) = attr(line, "URI") {
                m.audio = Some(join(base, uri)?);
            }
        } else if line.starts_with("#EXT-X-STREAM-INF:") {
            let res = attr(line, "RESOLUTION").and_then(|r| r.split_once('x'));
            let uri = lines[i + 1..].iter().find(|l| !l.is_empty() && !l.starts_with('#'));
            if let (Some((w, h)), Some(uri)) = (res, uri) {
                m.variants.push(Variant {
                    width: w.parse().unwrap_or(0),
                    height: h.parse().unwrap_or(0),
                    bandwidth: attr(line, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0),
                    url: join(base, uri)?,
                });
            }
        }
    }
    m.variants.sort_by_key(|v| std::cmp::Reverse((v.height, v.bandwidth)));
    Ok(m)
}

pub fn parse_media(base: &str, text: &str) -> Result<Media> {
    let mut media = Media::default();
    let (mut t, mut pending) = (0.0, None::<f64>);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("#EXT-X-MAP:") {
            media.init = attr(line, "URI").map(|u| join(base, u)).transpose()?;
        } else if let Some(d) = line.strip_prefix("#EXTINF:") {
            let d = d.split(',').next().unwrap_or("0");
            pending = Some(d.parse().map_err(|_| Error::Parse(format!("EXTINF «{d}»")))?);
        } else if !line.starts_with('#') {
            let duration = pending.take().unwrap_or(0.0);
            media.segments.push(Segment {
                url: join(base, line)?,
                start: t,
                duration,
            });
            t += duration;
        }
    }
    if media.segments.is_empty() {
        return Err(Error::Parse("media playlist without segments".into()));
    }
    Ok(media)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: &str = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"eng\",URI=\"a1/index.m3u8\"\n\
        #EXT-X-STREAM-INF:BANDWIDTH=300000,RESOLUTION=640x480,CODECS=\"vp09\",AUDIO=\"audio\"\nv1/index.m3u8\n\
        #EXT-X-STREAM-INF:BANDWIDTH=900000,RESOLUTION=1280x720,AUDIO=\"audio\"\nv2/index.m3u8\n";
    const MEDIA: &str = "#EXTM3U\n#EXT-X-MAP:URI=\"init/1.m4s\"\n#EXTINF:14.8,\nmedia/1.m4s\n#EXTINF:10.2,\nmedia/2.m4s\n#EXT-X-ENDLIST\n";

    #[test]
    fn master() {
        let m = parse_master("https://h/x/playlist.m3u8", MASTER).unwrap();
        assert_eq!(m.audio.as_deref(), Some("https://h/x/a1/index.m3u8"));
        assert_eq!(m.variants.iter().map(|v| v.height).collect::<Vec<_>>(), vec![720, 480]);
        assert_eq!(m.variants[0].url, "https://h/x/v2/index.m3u8");
    }

    #[test]
    fn media() {
        let m = parse_media("https://h/x/a1/index.m3u8", MEDIA).unwrap();
        assert_eq!(m.init.as_deref(), Some("https://h/x/a1/init/1.m4s"));
        assert_eq!(m.segments.len(), 2);
        assert_eq!(m.segments[1].url, "https://h/x/a1/media/2.m4s");
        assert!((m.segments[1].start - 14.8).abs() < 1e-9);
    }

    #[test]
    fn picks_best_or_highest_under_the_limit() {
        let v = |h| Variant {
            width: 0,
            height: h,
            bandwidth: 0,
            url: String::new(),
        };
        let vs = vec![v(720), v(480), v(240)];
        assert_eq!(pick(&vs, None).unwrap().height, 720);
        assert_eq!(pick(&vs, Some(500)).unwrap().height, 480);
        assert_eq!(pick(&vs, Some(100)).unwrap().height, 240);
    }
}

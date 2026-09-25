//! Recording metadata: the JSON behind a `…/record-new/<id>` link.
//!
//! A recording is a set of *media sessions* — one per participant's camera or screen — that
//! play **in parallel**, each starting at its own `relativeTime`. They cannot be concatenated,
//! only mixed on a shared timeline.

use std::collections::HashMap;

use serde_json::Value;

use crate::{Error, Result};

pub const DEFAULT_API: &str = "https://my.mts-link.ru";

#[derive(Debug, Clone)]
pub struct Track {
    /// 1-based, ordered by start time — what `--tracks 3,5` refers to.
    pub index: usize,
    pub id: u64,
    pub name: String,
    pub is_host: bool,
    /// Offset from the start of the recording, seconds.
    pub start: f64,
    pub duration: f64,
    pub hls: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Record {
    pub id: String,
    pub title: String,
    /// `YYYY-MM-DD`
    pub date: String,
    pub duration: f64,
    pub tracks: Vec<Track>,
}

pub fn record_id(link: &str) -> Result<String> {
    let tail = link.split("record-new/").nth(1).unwrap_or("");
    let id: String = tail.chars().take_while(char::is_ascii_digit).collect();
    if id.is_empty() {
        return Err(Error::Usage(format!("not a recording link: {link} (expected …/record-new/<id>)")));
    }
    Ok(id)
}

pub fn api_url(base: &str, id: &str) -> String {
    format!("{}/api/eventsessions/{id}/record?withoutCuts=false", base.trim_end_matches('/'))
}

fn nickname(user: &Value) -> Option<String> {
    let nick = user["nickname"].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
    nick.or_else(|| {
        let parts: Vec<&str> = ["name", "secondName"].iter().filter_map(|k| user[*k].as_str()).collect();
        (!parts.is_empty()).then(|| parts.join(" "))
    })
}

pub fn parse(id: &str, v: &Value) -> Result<Record> {
    let logs = v["eventLogs"]
        .as_array()
        .ok_or_else(|| Error::Parse("no eventLogs in the recording".into()))?;
    let owner = v["user"]["id"].as_u64();
    let (mut people, mut durations) = (HashMap::new(), HashMap::new());
    for e in logs {
        let (module, d) = (e["module"].as_str().unwrap_or(""), &e["data"]);
        if module.starts_with("conference") && d["user"].is_object() {
            if let Some(conf) = d["id"].as_u64() {
                people.insert(conf, (nickname(&d["user"]), d["user"]["id"].as_u64()));
            }
        } else if module == "mediasession.update" {
            if let (Some(s), Some(dur)) = (d["id"].as_u64(), d["duration"].as_f64()) {
                durations.insert(s, dur);
            }
        }
    }
    let mut tracks: Vec<Track> = logs
        .iter()
        .filter(|e| e["module"] == "mediasession.add" && e["data"]["id"].is_u64())
        .map(|e| {
            let d = &e["data"];
            let (name, user) = people
                .get(&d["stream"]["conference"]["id"].as_u64().unwrap_or(0))
                .cloned()
                .unwrap_or((None, None));
            let id = d["id"].as_u64().unwrap_or(0);
            Track {
                index: 0,
                id,
                name: name.unwrap_or_else(|| "participant".into()),
                is_host: owner.is_some() && user == owner,
                start: e["relativeTime"].as_f64().unwrap_or(0.0),
                duration: durations.get(&id).copied().unwrap_or(0.0),
                hls: d["hlsUrl"].as_str().map(str::to_owned),
            }
        })
        .collect();
    tracks.sort_by(|a, b| a.start.total_cmp(&b.start));
    tracks.iter_mut().enumerate().for_each(|(i, t)| t.index = i + 1);
    Ok(Record {
        id: id.to_owned(),
        title: v["name"].as_str().unwrap_or(id).to_owned(),
        date: v["createAt"].as_str().unwrap_or("").chars().take(10).collect(),
        duration: v["duration"].as_f64().unwrap_or(0.0),
        tracks,
    })
}

/// `all` / empty → every track; otherwise a comma list of `host` and 1-based indices.
pub fn select(tracks: &[Track], spec: Option<&str>) -> Result<Vec<Track>> {
    let spec = spec.map(str::trim).filter(|s| !s.is_empty() && *s != "all");
    let Some(spec) = spec else { return Ok(tracks.to_vec()) };
    let mut picked: Vec<Track> = Vec::new();
    for item in spec.split(',').map(str::trim) {
        let found: Vec<&Track> = if item == "host" {
            tracks.iter().filter(|t| t.is_host).collect()
        } else {
            let n: usize = item
                .parse()
                .map_err(|_| Error::Usage(format!("--tracks: «{item}» is neither host nor a number")))?;
            tracks.iter().filter(|t| t.index == n).collect()
        };
        if found.is_empty() {
            return Err(Error::Usage(format!("--tracks: no track «{item}» (see --list)")));
        }
        for t in found {
            if !picked.iter().any(|p| p.id == t.id) {
                picked.push(t.clone());
            }
        }
    }
    picked.sort_by_key(|t| t.index);
    Ok(picked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::json!({
            "name": "Webinar", "createAt": "2026-09-22T12:22:20+0300", "duration": 100.0, "user": {"id": 7},
            "eventLogs": [
                {"module": "conference.add", "data": {"id": 1, "user": {"id": 7, "nickname": "Host"}}},
                {"module": "conference.add", "data": {"id": 2, "user": {"id": 8, "name": "Ann", "secondName": "Lee"}}},
                {"module": "mediasession.add", "relativeTime": 30.0, "data": {"id": 11, "hlsUrl": "https://h/b.m3u8", "stream": {"conference": {"id": 2}}}},
                {"module": "mediasession.add", "relativeTime": 5.5, "data": {"id": 10, "hlsUrl": "https://h/a.m3u8", "stream": {"conference": {"id": 1}}}},
                {"module": "mediasession.update", "data": {"id": 10, "duration": 60.0}},
                {"module": "userlist.online", "data": []}
            ]
        })
    }

    #[test]
    fn parses_tracks_in_time_order() {
        let r = parse("42", &fixture()).unwrap();
        assert_eq!((r.title.as_str(), r.date.as_str(), r.duration), ("Webinar", "2026-09-22", 100.0));
        let t: Vec<_> = r.tracks.iter().map(|t| (t.index, t.id, t.name.as_str(), t.is_host)).collect();
        assert_eq!(t, vec![(1, 10, "Host", true), (2, 11, "Ann Lee", false)]);
        assert_eq!((r.tracks[0].start, r.tracks[0].duration, r.tracks[1].duration), (5.5, 60.0, 0.0));
    }

    #[test]
    fn selects_host_and_indices() {
        let r = parse("42", &fixture()).unwrap();
        let ids = |s| select(&r.tracks, Some(s)).unwrap().iter().map(|t| t.id).collect::<Vec<_>>();
        assert_eq!(ids("host"), vec![10]);
        assert_eq!(ids("2,host,2"), vec![10, 11]);
        assert_eq!(select(&r.tracks, None).unwrap().len(), 2);
        assert!(select(&r.tracks, Some("9")).is_err());
    }

    #[test]
    fn record_id_from_link() {
        assert_eq!(record_id("https://x/j/1/2/record-new/24554640907?x=1").unwrap(), "24554640907");
        assert!(record_id("https://x/j/1/2").is_err());
    }
}

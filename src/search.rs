use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::model::{Track, is_valid_video_id};

pub fn search_youtube(query: &str, canceled: impl Fn() -> bool) -> Result<Vec<Track>, String> {
    let query = query.trim();
    if canceled() {
        return Err("Search canceled".into());
    }
    if query.is_empty() {
        return Ok(Vec::new());
    }

    fetch_tracks(&format!("ytsearch20:{query}"), &[], canceled)
}

/// YouTube's generated mix, bounded because mix playlists can be endless.
pub fn radio_youtube(id: &str, canceled: impl Fn() -> bool) -> Result<Vec<Track>, String> {
    if !is_valid_video_id(id) {
        return Err("Cannot start radio for an invalid track.".into());
    }
    fetch_tracks(
        &format!("https://www.youtube.com/watch?v={id}&list=RD{id}"),
        &["--yes-playlist", "--playlist-end", "25"],
        canceled,
    )
    .map_err(|e| format!("Couldn't load song radio: {e}"))
}

fn fetch_tracks(
    url: &str,
    extra_args: &[&str],
    canceled: impl Fn() -> bool,
) -> Result<Vec<Track>, String> {
    if canceled() {
        return Err("Request canceled".into());
    }
    let mut child = Command::new("yt-dlp")
        .args([
            "--ignore-config",
            "--socket-timeout",
            "15",
            "--retries",
            "1",
            "--dump-single-json",
            "--flat-playlist",
            "--skip-download",
            "--no-warnings",
            "--ignore-errors",
        ])
        .args(extra_args)
        .args(["--", url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!("Unable to launch yt-dlp: {error}. Install yt-dlp and ensure it's on PATH.")
        })?;

    let stdout = child.stdout.take().ok_or("Missing search output")?;
    let stderr = child.stderr.take().ok_or("Missing search diagnostics")?;
    let read = |pipe: Box<dyn Read + Send>, limit| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            pipe.take(limit).read_to_end(&mut bytes).map(|_| bytes)
        })
    };
    let out = read(Box::new(stdout), 8 * 1024 * 1024);
    let err = read(Box::new(stderr), 64 * 1024);
    let start = Instant::now();
    let status = loop {
        if canceled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Search canceled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < Duration::from_secs(35) => {
                thread::sleep(Duration::from_millis(100))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Search took too long. Check your connection and try again.".into());
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        }
    };
    let stdout = out
        .join()
        .map_err(|_| "Search reader stopped")?
        .map_err(|e| e.to_string())?;
    let stderr = err
        .join()
        .map_err(|_| "Search reader stopped")?
        .map_err(|e| e.to_string())?;
    if !status.success() {
        let message = String::from_utf8_lossy(&stderr);
        let detail: String = message.trim().chars().take(500).collect();
        return Err(if detail.is_empty() {
            "Couldn't reach YouTube. Check your connection or update yt-dlp, then retry.".into()
        } else {
            format!("Search failed: {detail}")
        });
    }
    parse_search_results(&stdout)
}

pub fn parse_search_results(bytes: &[u8]) -> Result<Vec<Track>, String> {
    let json: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Invalid response from yt-dlp: {error}"))?;
    let entries = json["entries"]
        .as_array()
        .ok_or_else(|| "YouTube returned no searchable entries.".to_owned())?;

    Ok(entries
        .iter()
        .filter_map(|item| {
            let id = item["id"].as_str()?;
            if !is_valid_video_id(id) {
                return None;
            }
            let title = item["title"].as_str()?.trim();
            if title.is_empty() {
                return None;
            }
            let artist = item["channel"]
                .as_str()
                .or_else(|| item["uploader"].as_str())
                .or_else(|| item["artist"].as_str())
                .unwrap_or("Unknown artist")
                .trim();
            let duration_seconds = item["duration"].as_u64().or_else(|| {
                item["duration"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .map(|n| n as u64)
            });
            Some(Track {
                id: id.to_owned(),
                title: title.to_owned(),
                artist: artist.to_owned(),
                duration_seconds,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires yt-dlp and live YouTube access"]
    fn radio_fetches_live_mix() {
        let tracks = radio_youtube("4NRXx6U8ABQ", || false).unwrap();
        assert!(tracks.len() > 1 && tracks.len() <= 25);
        assert!(tracks.iter().any(|t| t.id != "4NRXx6U8ABQ"));
        assert!(
            tracks
                .iter()
                .all(|t| is_valid_video_id(&t.id) && !t.title.is_empty())
        );
    }
    #[test]
    fn radio_validates_seed_and_honors_cancellation_before_launching() {
        assert!(
            radio_youtube("invalid", || false)
                .unwrap_err()
                .contains("invalid track")
        );
        assert!(
            radio_youtube("dQw4w9WgXcQ", || true)
                .unwrap_err()
                .contains("canceled")
        );
    }
    #[test]
    fn parses_flat_search_results() {
        let bytes = br#"{"entries":[{"id":"dQw4w9WgXcQ","title":"Never Gonna Give You Up","channel":"Rick Astley","duration":213},{"id":"bad","title":"No"},null]}"#;
        let songs = parse_search_results(bytes).unwrap();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].artist, "Rick Astley");
        assert_eq!(songs[0].duration_label(), "3:33");
    }

    #[test]
    fn rejects_invalid_json() {
        assert!(parse_search_results(b"broken").is_err());
    }
}

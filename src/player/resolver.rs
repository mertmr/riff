use super::process::ChildProcess as Child;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub(super) struct Media {
    pub url: String,
    pub options: Value,
}

impl Media {
    pub fn plain(url: String) -> Self {
        Self {
            url,
            options: json!({}),
        }
    }

    fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let data: Value = serde_json::from_slice(bytes).map_err(|_| "Invalid audio response")?;
        let url = data["url"]
            .as_str()
            .filter(|url| {
                (url.starts_with("https://") || url.starts_with("http://"))
                    && !url.chars().any(char::is_control)
            })
            .ok_or("No playable audio stream")?;
        let mut headers = Vec::new();
        if let Some(values) = data["http_headers"].as_object() {
            for (name, value) in values {
                let Some(value) = value.as_str() else {
                    continue;
                };
                if !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !value.chars().any(char::is_control)
                {
                    // Preserve literal commas/backslashes in mpv's string list.
                    headers.push(format!(
                        "{name}: {}",
                        value.replace('\\', "\\\\").replace(',', "\\,")
                    ));
                }
            }
        }
        Ok(Self {
            url: url.to_owned(),
            options: json!({"ytdl":"no", "http-header-fields":headers.join(",")}),
        })
    }
}

// One cancellable extractor at a time. Only tiny URL/header metadata touches
// disk; the audio stays in mpv's bounded native streaming cache.
pub(super) struct Resolver {
    child: Child,
    output: PathBuf,
    started: Instant,
}
impl Resolver {
    pub fn start(url: &str) -> Result<Self, String> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let output = std::env::temp_dir().join(format!(
            "riff-resolve-{}-{}.json",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&output).map_err(|e| e.to_string())?;
        let child = Child::spawn(
            Command::new("yt-dlp")
                .args([
                    "--ignore-config",
                    "--no-playlist",
                    "--skip-download",
                    "--no-warnings",
                    "--no-progress",
                    "--no-cache-dir",
                    "--socket-timeout",
                    "10",
                    "--retries",
                    "1",
                    "--extractor-retries",
                    "1",
                    "--format",
                    "bestaudio/best",
                    "--print",
                    "%(.{url,http_headers})j",
                    "--",
                    url,
                ])
                .stdin(Stdio::null())
                .stdout(file)
                .stderr(Stdio::null()),
        );
        match child {
            Ok(child) => Ok(Self {
                child,
                output,
                started: Instant::now(),
            }),
            Err(error) => {
                let _ = fs::remove_file(output);
                Err(error.to_string())
            }
        }
    }
    pub fn poll(&mut self) -> Option<Result<Media, String>> {
        if self.started.elapsed() > Duration::from_secs(35)
            || fs::metadata(&self.output).is_ok_and(|m| m.len() > 64 * 1024)
        {
            return Some(Err("Audio preparation timed out".into()));
        }
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) if status.success() => Some(
                fs::read(&self.output)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| Media::from_json(&bytes)),
            ),
            _ => Some(Err("Couldn't prepare the next audio stream".into())),
        }
    }
}
impl Drop for Resolver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stream_headers_are_preserved_without_injection() {
        let media = Media::from_json(br#"{"url":"https://media.example/audio","http_headers":{"User-Agent":"a,b","Referer":"https://youtube.com/","bad\r\n":"x","Cookie":"x\r\ny"}}"#).unwrap();
        assert_eq!(media.options["ytdl"], "no");
        assert_eq!(
            media.options["http-header-fields"],
            "Referer: https://youtube.com/,User-Agent: a\\,b"
        );
        for bytes in [
            br#"{"url":"file:///secret"}"#.as_slice(),
            br#"{"url":"NA"}"#,
            b"invalid",
        ] {
            assert!(Media::from_json(bytes).is_err());
        }
    }
    #[test]
    #[ignore = "requires yt-dlp and live YouTube access"]
    fn live_audio_resolution_and_cancellation() {
        let mut resolver = Resolver::start("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap();
        let output = resolver.output.clone();
        let media = loop {
            if let Some(result) = resolver.poll() {
                break result.expect("live audio stream must resolve");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(media.url.starts_with("https://"));
        assert_eq!(media.options["ytdl"], "no");
        drop(resolver);
        assert!(!output.exists());
        let resolver = Resolver::start("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap();
        let output = resolver.output.clone();
        drop(resolver);
        assert!(!output.exists());
    }
}

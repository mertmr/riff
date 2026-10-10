use super::stream::{Server, Stream};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
const MAX_ENTRIES: usize = 32;

#[derive(Clone)]
pub(super) struct Media {
    pub url: String,
    pub options: Value,
    pub stream: Option<Arc<Stream>>,
}
impl Media {
    pub fn plain(url: String) -> Self {
        Self {
            url,
            options: json!({"ytdl":"no"}),
            stream: None,
        }
    }
    fn valid(&self) -> bool {
        self.stream
            .as_ref()
            .is_none_or(|s| Instant::now() < s.expires && !s.rejected.load(Ordering::Relaxed))
    }
}
struct Slot {
    id: String,
    cancelled: Arc<AtomicBool>,
    result: Mutex<Option<Result<Media, String>>>,
    started: AtomicBool,
    prefix_done: AtomicBool,
}
pub(super) struct Resolver(Arc<Slot>);
impl Resolver {
    pub fn poll_prepared(&mut self) -> Option<Result<Media, String>> {
        let result = self.poll()?;
        if let Ok(media) = &result
            && let Some(stream) = &media.stream
            && !stream.prefix_ready()
        {
            return self
                .0
                .prefix_done
                .load(Ordering::Acquire)
                .then(|| Err("Opening audio could not be prepared".into()));
        }
        Some(result)
    }
    pub fn poll(&mut self) -> Option<Result<Media, String>> {
        self.0.result.lock().unwrap().clone()
    }
}
struct State {
    entries: HashMap<String, Arc<Slot>>,
    lru: VecDeque<String>,
    foreground: Option<String>,
    upcoming: Vec<String>,
    shutdown: bool,
}
impl State {
    fn wanted(&self, id: &str) -> bool {
        self.foreground.as_deref() == Some(id) || self.upcoming.iter().any(|i| i == id)
    }
    fn request(&mut self, id: &str) -> Resolver {
        self.lru.retain(|i| i != id);
        self.lru.push_back(id.into());
        let reusable = self.entries.get(id).is_some_and(|s| {
            let result = s.result.lock().unwrap();
            matches!(&*result, Some(Ok(media)) if media.valid())
                || (result.is_none() && !s.cancelled.load(Ordering::Relaxed))
        });
        if !reusable {
            if let Some(old) = self.entries.remove(id) {
                old.cancelled.store(true, Ordering::Relaxed);
            }
            self.entries.insert(
                id.into(),
                Arc::new(Slot {
                    id: id.into(),
                    cancelled: Arc::new(AtomicBool::new(false)),
                    result: Mutex::new(None),
                    started: AtomicBool::new(false),
                    prefix_done: AtomicBool::new(false),
                }),
            );
        }
        let slot = self.entries[id].clone();
        slot.cancelled.store(false, Ordering::Relaxed);
        while self.entries.len() > MAX_ENTRIES {
            let candidate = self.lru.iter().find(|i| !self.wanted(i)).cloned();
            let Some(id) = candidate else { break };
            self.lru.retain(|i| i != &id);
            if let Some(s) = self.entries.remove(&id) {
                s.cancelled.store(true, Ordering::Relaxed);
            }
        }
        Resolver(slot)
    }
    fn select(&mut self, id: &str, upcoming: Vec<String>) -> Resolver {
        self.foreground = Some(id.into());
        self.upcoming = upcoming.into_iter().take(4).collect();
        let ticket = self.request(id);
        for id in self.upcoming.clone() {
            self.request(&id);
        }
        self.cancel_unwanted();
        ticket
    }
    fn cancel_unwanted(&self) {
        for (id, slot) in &self.entries {
            if !self.wanted(id) {
                slot.cancelled.store(true, Ordering::Relaxed);
            }
        }
    }
    fn job(&self, speculative: bool) -> Option<Arc<Slot>> {
        let ids: Vec<&str> = if speculative {
            self.foreground
                .iter()
                .chain(self.upcoming.iter())
                .map(String::as_str)
                .collect()
        } else {
            self.foreground.iter().map(String::as_str).collect()
        };
        ids.into_iter().find_map(|id| {
            let s = self.entries.get(id)?;
            if s.cancelled.load(Ordering::Relaxed) || s.result.lock().unwrap().is_some() {
                return None;
            }
            (!s.started.swap(true, Ordering::Relaxed)).then(|| s.clone())
        })
    }
}
// Fixed workers and a single pooled agent. One worker is reserved for selections;
// prefetch never occupies both workers. Promotion reuses the same slot/result.
pub(super) struct Service {
    state: Arc<(Mutex<State>, Condvar)>,
    workers: Vec<thread::JoinHandle<()>>,
    _server: Arc<Server>,
}
impl Service {
    pub fn new(directory: Option<PathBuf>) -> Result<Self, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(4)))
            .build()
            .into();
        let server = Arc::new(Server::new(agent.clone(), directory).map_err(|e| e.to_string())?);
        let state = Arc::new((
            Mutex::new(State {
                entries: HashMap::new(),
                lru: VecDeque::new(),
                foreground: None,
                upcoming: Vec::new(),
                shutdown: false,
            }),
            Condvar::new(),
        ));
        let visitor = Arc::new(Mutex::new(None::<String>));
        let mut service = Self {
            state: state.clone(),
            workers: Vec::new(),
            _server: server.clone(),
        };
        for speculative in [false, true] {
            let state = state.clone();
            let server = server.clone();
            let agent = agent.clone();
            let visitor = visitor.clone();
            service.workers.push(
                thread::Builder::new()
                    .name(
                        if speculative {
                            "audio-prepare"
                        } else {
                            "audio-resolve"
                        }
                        .into(),
                    )
                    .spawn(move || {
                        loop {
                            let slot = {
                                let (lock, changed) = &*state;
                                let mut state = lock.lock().unwrap();
                                loop {
                                    if state.shutdown {
                                        return;
                                    }
                                    if let Some(job) = state.job(speculative) {
                                        break job;
                                    }
                                    state = changed.wait(state).unwrap();
                                }
                            };
                            let result = resolve(&agent, &visitor, &slot).map(|stream| {
                                let stream = Arc::new(stream);
                                Media {
                                    url: server.register(stream.clone()),
                                    options: json!({"ytdl":"no"}),
                                    stream: Some(stream),
                                }
                            });
                            if slot.cancelled.load(Ordering::Relaxed) {
                                continue;
                            }
                            *slot.result.lock().unwrap() = Some(result.clone());
                            if let Ok(media) = result {
                                let foreground =
                                    state.0.lock().unwrap().foreground.as_deref() == Some(&slot.id);
                                if !foreground && let Some(stream) = media.stream {
                                    let _ = stream.prepare(&agent);
                                    slot.prefix_done.store(true, Ordering::Release);
                                }
                            }
                        }
                    })
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(service)
    }
    pub fn select(&self, id: &str, upcoming: Vec<String>) -> Resolver {
        let mut state = self.state.0.lock().unwrap();
        // Selection and its new queue window are applied together so useful
        // in-flight preparation survives when the queue advances.
        let ticket = state.select(id, upcoming);
        self.state.1.notify_all();
        ticket
    }
    pub fn prepare(&self, id: &str) -> Resolver {
        let mut state = self.state.0.lock().unwrap();
        if !state.upcoming.iter().any(|i| i == id) && state.upcoming.len() < 4 {
            state.upcoming.push(id.into());
        }
        let ticket = state.request(id);
        self.state.1.notify_all();
        ticket
    }
    pub fn window(&self, ids: Vec<String>) {
        let mut state = self.state.0.lock().unwrap();
        state.upcoming = ids.into_iter().take(4).collect();
        for id in state.upcoming.clone() {
            state.request(&id);
        }
        state.cancel_unwanted();
        self.state.1.notify_all();
    }
    pub fn stop(&self) {
        let mut state = self.state.0.lock().unwrap();
        state.foreground = None;
        state.upcoming.clear();
        state.cancel_unwanted();
    }
    pub fn invalidate(&self, id: &str) {
        let mut state = self.state.0.lock().unwrap();
        state.lru.retain(|entry| entry != id);
        if let Some(s) = state.entries.remove(id) {
            s.cancelled.store(true, Ordering::Relaxed);
        }
    }
    pub fn playing(&self, id: &str) {
        let mut state = self.state.0.lock().unwrap();
        state.foreground = Some(id.into());
        state.upcoming.retain(|i| i != id);
        // Keep the previous stream alive until mpv has drained its output tail.
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        {
            let mut state = self.state.0.lock().unwrap();
            state.shutdown = true;
            for s in state.entries.values() {
                s.cancelled.store(true, Ordering::Relaxed);
            }
        }
        self.state.1.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        // Server workers terminate after active bounded range requests return.
    }
}
fn resolve(
    agent: &ureq::Agent,
    session: &Mutex<Option<String>>,
    slot: &Slot,
) -> Result<Stream, String> {
    if !crate::model::is_valid_video_id(&slot.id) {
        return Err("Invalid track ID".into());
    }
    let visitor = {
        let mut visitor = session.lock().unwrap();
        if visitor.is_none() {
            let page = agent
                .get("https://www.youtube.com/")
                .header("User-Agent", USER_AGENT)
                .call()
                .map_err(|e| e.to_string())?
                .body_mut()
                .with_config()
                .limit(4 * 1024 * 1024)
                .read_to_string()
                .map_err(|e| e.to_string())?;
            *visitor = Some(visitor_data(&page).ok_or("YouTube session unavailable")?);
        }
        visitor.clone().unwrap()
    };
    if slot.cancelled.load(Ordering::Relaxed) {
        return Err("Superseded selection".into());
    }
    let payload = json!({"context":{"client":{"clientName":"VISIONOS","clientVersion":"0.1","visitorData":visitor,"userAgent":USER_AGENT,"hl":"en","gl":"US","osName":"visionOS","osVersion":"1.3.21O771","deviceMake":"Apple","deviceModel":"RealityDevice14,1"}},"videoId":slot.id,"contentCheckOk":true,"racyCheckOk":true,"playbackContext":{"contentPlaybackContext":{"html5Preference":"HTML5_PREF_WANTS"}}});
    let mut response = agent
        .post("https://www.youtube.com/youtubei/v1/player")
        .header("Content-Type", "application/json")
        .header("Origin", "https://www.youtube.com")
        .header("User-Agent", USER_AGENT)
        .header("X-Youtube-Client-Name", "101")
        .header("X-Youtube-Client-Version", "0.1")
        .header("X-Goog-Visitor-Id", &visitor)
        .config()
        .http_status_as_error(false)
        .build()
        .send(payload.to_string())
        .map_err(|e| format!("Couldn't resolve audio: {e}"))?;
    let status = response.status().as_u16();
    let bytes = response
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    if slot.cancelled.load(Ordering::Relaxed) {
        return Err("Superseded selection".into());
    }
    let data: Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid player response")?;
    if status != 200 {
        if matches!(status, 400 | 403) {
            let mut cached = session.lock().unwrap();
            if cached.as_ref() == Some(&visitor) {
                *cached = None;
            }
        }
        return Err(format!(
            "YouTube player HTTP {status}: {}",
            data["error"]["message"]
                .as_str()
                .unwrap_or("request rejected")
        ));
    }
    let stream = parse_stream(&data, &slot.id, slot.cancelled.clone())?;
    // Reject URLs that only serve an opening preview before handing them to mpv.
    stream.fetch(agent, stream.length - 1, stream.length - 1)?;
    Ok(stream)
}
fn visitor_data(page: &str) -> Option<String> {
    let (_, tail) = page.split_once("\"VISITOR_DATA\":")?;
    let mut values = serde_json::Deserializer::from_str(tail.trim_start()).into_iter::<String>();
    values
        .next()?
        .ok()
        .filter(|v| !v.is_empty() && v.len() <= 1024)
}
fn parse_stream(data: &Value, id: &str, cancelled: Arc<AtomicBool>) -> Result<Stream, String> {
    if data["playabilityStatus"]["status"] != "OK" {
        return Err(data["playabilityStatus"]["reason"]
            .as_str()
            .unwrap_or("This track is unavailable")
            .into());
    }
    let format = data["streamingData"]["adaptiveFormats"]
        .as_array()
        .ok_or("No audio formats")?
        .iter()
        .filter(|f| {
            f["mimeType"]
                .as_str()
                .is_some_and(|m| m.starts_with("audio/"))
                && f["url"].as_str().is_some_and(valid_url)
                && f["contentLength"]
                    .as_str()
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n > 0 && n <= 1024 * 1024 * 1024)
        })
        .max_by_key(|f| f["bitrate"].as_u64().unwrap_or(0))
        .ok_or("No directly playable audio format")?;
    let ttl = data["streamingData"]["expiresInSeconds"]
        .as_str()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(300)
        .clamp(1, 6 * 3600);
    let url = format["url"].as_str().unwrap();
    let signed_expiry = url.split_once('?').and_then(|(_, query)| {
        query.split('&').find_map(|p| {
            p.strip_prefix("expire=")
                .and_then(|n| n.parse::<u64>().ok())
        })
    });
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|_| "Invalid system clock")?
        .as_secs();
    let ttl = ttl.min(signed_expiry.map_or(ttl, |expiry| expiry.saturating_sub(now)));
    if ttl == 0 {
        return Err("The audio URL has already expired".into());
    }
    // Keep a margin for in-flight requests at the signed URL's expiry boundary.
    let ttl = ttl.saturating_sub(60).max(1);
    Ok(Stream::new(
        id.into(),
        url.into(),
        USER_AGENT.into(),
        format["contentLength"].as_str().unwrap().parse().unwrap(),
        Instant::now() + Duration::from_secs(ttl),
        cancelled,
    ))
}
fn valid_url(url: &str) -> bool {
    if url.chars().any(char::is_control) {
        return false;
    }
    url.strip_prefix("https://")
        .and_then(|s| s.split('/').next())
        .is_some_and(|host| {
            host.ends_with(".googlevideo.com") && !host.contains(['@', ':', '?', '#'])
        })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_audio_only_and_rejects_unsafe_urls_and_cipher_only_formats() {
        let data = json!({"playabilityStatus":{"status":"OK"},"streamingData":{"expiresInSeconds":"3600","adaptiveFormats":[{"mimeType":"video/mp4","url":"https://r.googlevideo.com/video","bitrate":900000,"contentLength":"10"},{"mimeType":"audio/webm","url":"https://r.googlevideo.com/audio","bitrate":120000,"contentLength":"100"},{"mimeType":"audio/mp4","signatureCipher":"x","bitrate":200000,"contentLength":"100"}]}});
        let stream = parse_stream(&data, "hTWKbfoikeg", Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(stream.url, "https://r.googlevideo.com/audio");
        assert_eq!(stream.length, 100);
        let mut expired = data.clone();
        expired["streamingData"]["adaptiveFormats"][1]["url"] =
            json!("https://r.googlevideo.com/audio?expire=1");
        assert!(parse_stream(&expired, "hTWKbfoikeg", Arc::new(AtomicBool::new(false))).is_err());
        assert!(stream.expires.duration_since(Instant::now()) < Duration::from_secs(3600));
        for url in [
            "https://googlevideo.com.evil/x",
            "https://r.googlevideo.com@evil/x",
            "http://r.googlevideo.com/x",
            "https://r.googlevideo.com:443/x",
            "https://r.googlevideo.com/a\r\n",
        ] {
            assert!(!valid_url(url));
        }
        assert_eq!(
            visitor_data(r#"abc"VISITOR_DATA":"abc\u003d",other"#),
            Some("abc=".into())
        );
    }
    #[test]
    fn promotion_shares_work_and_old_selections_are_cancelled() {
        let mut state = State {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            foreground: None,
            upcoming: vec!["a".into()],
            shutdown: false,
        };
        let next = state.request("a");
        let job = state.job(true).unwrap();
        state.foreground = Some("a".into());
        let foreground = state.request("a");
        assert!(Arc::ptr_eq(&next.0, &foreground.0));
        assert!(Arc::ptr_eq(&job, &foreground.0));
        assert!(state.job(false).is_none());
        state.foreground = Some("b".into());
        state.upcoming.clear();
        state.request("b");
        state.cancel_unwanted();
        assert!(job.cancelled.load(Ordering::Relaxed));
        assert_eq!(state.job(false).unwrap().id, "b");
    }
    #[test]
    fn queue_advancement_preserves_useful_in_flight_preparation() {
        let mut state = State {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            foreground: None,
            upcoming: Vec::new(),
            shutdown: false,
        };
        state.select("a", vec!["b".into(), "c".into()]);
        state.entries["a"].started.store(true, Ordering::Relaxed);
        let b = state.job(true).unwrap();
        let c = state.job(true).unwrap();
        let selected = state.select("b", vec!["c".into(), "d".into()]);
        assert!(Arc::ptr_eq(&selected.0, &b));
        assert!(Arc::ptr_eq(&state.entries["c"], &c));
        assert!(!c.cancelled.load(Ordering::Relaxed));
        assert!(state.entries["a"].cancelled.load(Ordering::Relaxed));
    }
    #[test]
    fn speculative_jobs_leave_the_foreground_worker_free_and_cache_is_bounded() {
        let mut state = State {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            foreground: None,
            upcoming: vec!["next".into()],
            shutdown: false,
        };
        state.request("next");
        assert!(state.job(false).is_none());
        assert_eq!(state.job(true).unwrap().id, "next");
        for i in 0..100 {
            state.foreground = Some(i.to_string());
            state.request(&i.to_string());
            state.cancel_unwanted();
        }
        assert_eq!(state.entries.len(), MAX_ENTRIES);
        assert!(state.entries.contains_key("99"));
    }
}

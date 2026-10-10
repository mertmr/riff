use crate::audio_cache::AudioCache;
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub(super) const PREFIX_BYTES: usize = 96 * 1024;
const CHUNK_BYTES: usize = 256 * 1024;
const MAX_STREAMS: usize = 64;

pub(super) struct Stream {
    pub id: String,
    pub url: String,
    pub user_agent: String,
    pub length: u64,
    pub expires: Instant,
    pub cancelled: Arc<AtomicBool>,
    pub rejected: AtomicBool,
    prefix: Mutex<Arc<Vec<u8>>>,
    first_byte: Mutex<Option<Instant>>,
}
impl Stream {
    pub fn new(
        id: String,
        url: String,
        user_agent: String,
        length: u64,
        expires: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            id,
            url,
            user_agent,
            length,
            expires,
            cancelled,
            rejected: AtomicBool::new(false),
            prefix: Mutex::new(Arc::new(Vec::new())),
            first_byte: Mutex::new(None),
        }
    }
    pub fn first_byte(&self) -> Option<Instant> {
        *self.first_byte.lock().unwrap()
    }
    pub fn prefix_ready(&self) -> bool {
        !self.prefix.lock().unwrap().is_empty()
    }
    pub fn fetch(&self, agent: &ureq::Agent, start: u64, end: u64) -> Result<Vec<u8>, String> {
        if start > end || end >= self.length || end - start + 1 > CHUNK_BYTES as u64 {
            return Err("Invalid upstream range".into());
        }
        if self.cancelled.load(Ordering::Relaxed)
            || self.rejected.load(Ordering::Relaxed)
            || Instant::now() >= self.expires
        {
            return Err("Cancelled or expired audio stream".into());
        }
        let range = format!("bytes={start}-{end}");
        let mut response = agent
            .get(&self.url)
            .header("User-Agent", &self.user_agent)
            .header("Range", &range)
            .call()
            .map_err(|e| {
                // A rejected URL must be re-resolved, never retried from the URL cache.
                if matches!(&e, ureq::Error::StatusCode(403 | 410)) {
                    self.rejected.store(true, Ordering::Relaxed);
                }
                format!("Audio range request failed: {e}")
            })?;
        let expected = format!("bytes {start}-{end}/{}", self.length);
        if response.status().as_u16() != 206
            || response
                .headers()
                .get("Content-Range")
                .and_then(|h| h.to_str().ok())
                != Some(expected.as_str())
        {
            self.rejected.store(true, Ordering::Relaxed);
            return Err("The server did not honor the audio range".into());
        }
        let mut bytes = Vec::with_capacity((end - start + 1) as usize);
        let mut reader = response.body_mut().as_reader().take(CHUNK_BYTES as u64 + 1);
        let mut first = [0];
        reader.read_exact(&mut first).map_err(|e| e.to_string())?;
        if start == 0 {
            self.first_byte
                .lock()
                .unwrap()
                .get_or_insert_with(Instant::now);
        }
        bytes.push(first[0]);
        let mut chunk = [0; 8192];
        loop {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err("Superseded stream".into());
            }
            let count = reader.read(&mut chunk).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        if bytes.len() as u64 != end - start + 1 {
            return Err("Incomplete audio range".into());
        }

        if start == 0 {
            let mut prefix = self.prefix.lock().unwrap();
            if bytes.len().min(PREFIX_BYTES) > prefix.len() {
                *prefix = Arc::new(bytes[..bytes.len().min(PREFIX_BYTES)].to_vec());
            }
        }
        Ok(bytes)
    }
    pub fn prepare(&self, agent: &ureq::Agent) -> Result<(), String> {
        // Opening bytes are the only speculative media download.
        self.fetch(agent, 0, (PREFIX_BYTES as u64).min(self.length) - 1)?;
        Ok(())
    }
}

struct Registry {
    entries: HashMap<u64, Weak<Stream>>,
    sequence: u64,
}
pub(super) struct Server {
    address: SocketAddr,
    registry: Arc<Mutex<Registry>>,
    shutdown: Arc<AtomicBool>,
    workers: Vec<thread::JoinHandle<()>>,
}
impl Server {
    pub fn new(agent: ureq::Agent, directory: Option<PathBuf>) -> std::io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let registry = Arc::new(Mutex::new(Registry {
            entries: HashMap::new(),
            sequence: 0,
        }));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<TcpStream>(4);
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::new();
        // Current audio and mpv's gapless prefetch can stream independently.
        for n in 0..2 {
            let rx = rx.clone();
            let registry = registry.clone();
            let agent = agent.clone();
            let directory = directory.clone();
            let shutdown = shutdown.clone();
            workers.push(
                thread::Builder::new()
                    .name(format!("audio-stream-{n}"))
                    .spawn(move || {
                        while let Ok(mut socket) = { rx.lock().unwrap().recv() } {
                            if shutdown.load(Ordering::Relaxed) {
                                break;
                            }
                            let _ = socket.set_read_timeout(Some(Duration::from_secs(2)));
                            let _ = socket.set_nodelay(true);
                            let _ = socket.set_write_timeout(Some(Duration::from_millis(100)));
                            let _ = serve(&mut socket, &registry, &agent, directory.clone());
                        }
                    })?,
            );
        }
        let stop = shutdown.clone();
        workers.push(
            thread::Builder::new()
                .name("audio-listener".into())
                .spawn(move || {
                    for socket in listener.incoming() {
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        match socket {
                            Ok(socket) => {
                                let _ = tx.try_send(socket);
                            }
                            Err(_) => break,
                        }
                    }
                })?,
        );
        Ok(Self {
            address,
            registry,
            shutdown,
            workers,
        })
    }
    pub fn register(&self, stream: Arc<Stream>) -> String {
        let mut registry = self.registry.lock().unwrap();
        registry.entries.retain(|_, weak| {
            weak.upgrade()
                .is_some_and(|s| Instant::now() < s.expires && !s.rejected.load(Ordering::Relaxed))
        });
        while registry.entries.len() >= MAX_STREAMS {
            if let Some(key) = registry
                .entries
                .iter()
                .filter(|(_, weak)| {
                    weak.upgrade()
                        .is_none_or(|s| s.cancelled.load(Ordering::Relaxed))
                })
                .map(|(k, _)| *k)
                .min()
            {
                registry.entries.remove(&key);
            } else if let Some(key) = registry.entries.keys().copied().min() {
                registry.entries.remove(&key);
            }
        }
        registry.sequence = registry.sequence.wrapping_add(1);
        let key = registry.sequence;
        registry.entries.insert(key, Arc::downgrade(&stream));
        format!("http://{}/stream/{key}", self.address)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        for s in self
            .registry
            .lock()
            .unwrap()
            .entries
            .values()
            .filter_map(Weak::upgrade)
        {
            s.cancelled.store(true, Ordering::Relaxed);
        }
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(100));
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn parse_range(header: Option<&str>, length: u64) -> Result<(u64, u64), String> {
    if length == 0 {
        return Err("Empty audio".into());
    }
    let Some(header) = header else {
        return Ok((0, length - 1));
    };
    let (start, end) = header
        .strip_prefix("bytes=")
        .and_then(|r| r.split_once('-'))
        .ok_or("Invalid audio range")?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| "Invalid suffix")?;
        if suffix == 0 {
            return Err("Empty suffix".into());
        }
        return Ok((length.saturating_sub(suffix), length - 1));
    }
    let start = start.parse::<u64>().map_err(|_| "Invalid range start")?;
    let end = if end.is_empty() {
        length - 1
    } else {
        end.parse::<u64>()
            .map_err(|_| "Invalid range end")?
            .min(length - 1)
    };
    if start > end {
        return Err("Unsatisfiable audio range".into());
    }
    Ok((start, end))
}
fn serve(
    socket: &mut TcpStream,
    registry: &Mutex<Registry>,
    agent: &ureq::Agent,
    directory: Option<PathBuf>,
) -> Result<(), String> {
    let mut header = Vec::with_capacity(1024);
    // Connection-close requests have a bounded header; ignore trailing data.
    let mut chunk = [0; 1024];
    while !header.windows(4).any(|w| w == b"\r\n\r\n") && header.len() < 8192 {
        let remaining = (8192 - header.len()).min(chunk.len());
        let count = socket
            .read(&mut chunk[..remaining])
            .map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("Incomplete request".into());
        }
        header.extend_from_slice(&chunk[..count]);
    }
    let end = header
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("Oversized request")?
        + 4;
    let header = std::str::from_utf8(&header[..end]).map_err(|_| "Invalid request")?;
    let mut lines = header.lines();
    let mut request = lines.next().ok_or("Missing request")?.split_whitespace();
    let method = request.next().ok_or("Missing method")?;
    if !matches!(method, "GET" | "HEAD") {
        return Err("Unsupported method".into());
    }
    let key = request
        .next()
        .and_then(|p| p.strip_prefix("/stream/"))
        .and_then(|p| p.parse::<u64>().ok())
        .ok_or("Unknown stream")?;
    let stream = registry
        .lock()
        .unwrap()
        .entries
        .get(&key)
        .and_then(Weak::upgrade)
        .ok_or("Expired stream")?;
    if stream.cancelled.load(Ordering::Relaxed)
        || stream.rejected.load(Ordering::Relaxed)
        || Instant::now() >= stream.expires
    {
        let _ = socket
            .write_all(b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return Ok(());
    }
    let range = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("range").then_some(value.trim())
    });
    let (mut start, end) = match parse_range(range, stream.length) {
        Ok(range) => range,
        Err(_) => {
            write!(socket, "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", stream.length).map_err(|e| e.to_string())?;
            return Ok(());
        }
    };
    let first_start = start;
    let prefix = stream.prefix.lock().unwrap().clone();
    // Validate/fetch before writing success headers. A rejected signed URL must
    // fail loading, not appear to be an empty successful track.
    let first = if method == "GET" && start >= prefix.len() as u64 {
        match stream.fetch(
            agent,
            start,
            end.min(
                start
                    + if start == 0 {
                        PREFIX_BYTES as u64
                    } else {
                        CHUNK_BYTES as u64
                    }
                    - 1,
            ),
        ) {
            Ok(bytes) => Some(bytes),
            Err(error) => {
                let _ = socket.write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                return Err(error);
            }
        }
    } else {
        None
    };
    write!(socket, "HTTP/1.1 {}\r\nContent-Type: application/octet-stream\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n{}Connection: close\r\n\r\n", if range.is_some() { "206 Partial Content" } else { "200 OK" }, end - start + 1, if range.is_some() { format!("Content-Range: bytes {start}-{end}/{}\r\n", stream.length) } else { String::new() }).map_err(|e| e.to_string())?;
    if method == "HEAD" {
        return Ok(());
    }
    // Deliver opening audio before touching disk. Backpressure is expected
    // when mpv pauses or fills its cache, and must not truncate the stream.
    let opening = if start < prefix.len() as u64 {
        let count = (end - start + 1).min(prefix.len() as u64 - start) as usize;
        &prefix[start as usize..start as usize + count]
    } else {
        first.as_ref().unwrap().as_slice()
    };
    write_audio(socket, opening, &stream)?;
    let mut writer = (first_start == 0 && end + 1 == stream.length)
        .then(|| CacheWriter::new(directory, &stream.id))
        .flatten();
    if let Some(w) = &mut writer {
        w.write(opening);
    }
    start += opening.len() as u64;
    while start <= end {
        if stream.cancelled.load(Ordering::Relaxed) {
            return Err("Superseded stream".into());
        }
        let bytes = stream.fetch(agent, start, end.min(start + CHUNK_BYTES as u64 - 1))?;
        write_audio(socket, &bytes, &stream)?;
        if let Some(w) = &mut writer {
            w.write(&bytes);
        }
        start += bytes.len() as u64;
    }
    if let Some(writer) = writer {
        writer.finish(stream.length);
    }
    Ok(())
}
fn write_audio(socket: &mut TcpStream, mut bytes: &[u8], stream: &Stream) -> Result<(), String> {
    while !bytes.is_empty() {
        if stream.cancelled.load(Ordering::Relaxed) {
            return Err("Superseded stream".into());
        }
        match socket.write(bytes) {
            Ok(0) => return Err("Audio connection closed".into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}
struct CacheWriter {
    cache: AudioCache,
    dir: PathBuf,
    file: Option<File>,
    id: String,
    written: u64,
}
impl CacheWriter {
    fn new(directory: Option<PathBuf>, id: &str) -> Option<Self> {
        if !crate::model::is_valid_video_id(id) {
            return None;
        }
        let root = directory?;
        // This handle only publishes/evicts; no duplicate extractor or downloader.
        let cache = AudioCache::publisher(root.clone());
        if cache.lookup(id).is_some() {
            return None;
        }
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let dir = root.join(format!(
            "download-{}-native-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).ok()?;
        let file = match File::create(dir.join("audio.native")) {
            Ok(f) => f,
            Err(_) => {
                let _ = fs::remove_dir_all(&dir);
                return None;
            }
        };
        Some(Self {
            cache,
            dir,
            file: Some(file),
            id: id.into(),
            written: 0,
        })
    }
    fn write(&mut self, bytes: &[u8]) {
        if self.written + bytes.len() as u64 > 64 * 1024 * 1024 {
            self.file = None;
            return;
        }
        if let Some(file) = &mut self.file {
            if file.write_all(bytes).is_ok() {
                self.written += bytes.len() as u64;
            } else {
                self.file = None;
            }
        }
    }
    fn finish(mut self, length: u64) {
        if self.written != length {
            return;
        }
        if let Some(mut file) = self.file.take()
            && file.flush().is_ok()
        {
            drop(file);
            let _ = self.cache.publish(&self.dir, &self.id);
        }
    }
}
impl Drop for CacheWriter {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Origin {
        address: SocketAddr,
        requests: Arc<Mutex<Vec<(u64, u64)>>>,
        fail_after_prefix: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
        bytes: Arc<Vec<u8>>,
    }
    impl Origin {
        fn new() -> Self {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let fail_after_prefix = Arc::new(AtomicBool::new(false));
            let stop = Arc::new(AtomicBool::new(false));
            let bytes = Arc::new((0..600 * 1024).map(|i| (i % 251) as u8).collect::<Vec<_>>());
            let data = bytes.clone();
            let calls = requests.clone();
            let stopping = stop.clone();
            let fail = fail_after_prefix.clone();
            let worker = thread::spawn(move || {
                for socket in listener.incoming() {
                    if stopping.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut socket = socket.unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut header = Vec::new();
                    let mut chunk = [0; 1024];
                    while !header.windows(4).any(|w| w == b"\r\n\r\n") {
                        let count = socket.read(&mut chunk).unwrap();
                        if count == 0 {
                            break;
                        }
                        header.extend_from_slice(&chunk[..count]);
                    }
                    let header = String::from_utf8(header).unwrap();
                    let range = header.lines().find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("range").then_some(value.trim())
                    });
                    let (start, end) = parse_range(range, data.len() as u64).unwrap();
                    calls.lock().unwrap().push((start, end));
                    if fail.load(Ordering::Relaxed) && start >= PREFIX_BYTES as u64 {
                        let _ = socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        continue;
                    }
                    let _ = write!(
                        socket,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len(),
                        end - start + 1
                    );
                    let _ = socket.write_all(&data[start as usize..=end as usize]);
                }
            });
            Self {
                address,
                requests,
                fail_after_prefix,
                stop,
                worker: Some(worker),
                bytes,
            }
        }
        fn stream(&self) -> Arc<Stream> {
            Arc::new(Stream::new(
                "hTWKbfoikeg".into(),
                format!("http://{}/audio", self.address),
                "test".into(),
                self.bytes.len() as u64,
                Instant::now() + Duration::from_secs(60),
                Arc::new(AtomicBool::new(false)),
            ))
        }
    }
    impl Drop for Origin {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.address);
            self.worker.take().unwrap().join().unwrap();
        }
    }
    fn directory() -> PathBuf {
        std::env::temp_dir().join(format!(
            "riff-stream-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    #[test]
    fn prepared_ranges_seek_without_redownloading_and_only_complete_audio_is_published() {
        let origin = Origin::new();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(2)))
            .build()
            .into();
        let root = directory();
        let cache = AudioCache::at(Some(root.clone()));
        let server = Server::new(agent.clone(), Some(root.clone())).unwrap();
        let stream = origin.stream();
        stream.prepare(&agent).unwrap();
        let url = server.register(stream.clone());
        let bytes = agent
            .get(&url)
            .header("Range", "bytes=0-32767")
            .call()
            .unwrap()
            .body_mut()
            .read_to_vec()
            .unwrap();
        assert_eq!(bytes, origin.bytes[..32768]);
        assert_eq!(origin.requests.lock().unwrap().len(), 1);
        assert!(cache.lookup(&stream.id).is_none());
        let start = stream.length - 32;
        let bytes = agent
            .get(&url)
            .header("Range", &format!("bytes={start}-"))
            .call()
            .unwrap()
            .body_mut()
            .read_to_vec()
            .unwrap();
        assert_eq!(bytes, origin.bytes[start as usize..]);
        assert!(cache.lookup(&stream.id).is_none());
        let bytes = agent
            .get(&url)
            .call()
            .unwrap()
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_vec()
            .unwrap();
        assert_eq!(&bytes, &*origin.bytes);
        let deadline = Instant::now() + Duration::from_secs(2);
        while cache.lookup(&stream.id).is_none() {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert_eq!(fs::read(cache.lookup(&stream.id).unwrap()).unwrap(), bytes);
        assert!(
            origin
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(start, end)| end - start < CHUNK_BYTES as u64)
        );
        // A tiny subsequent header read must not replace a larger cached prefix.
        stream.fetch(&agent, 0, 1).unwrap();
        assert_eq!(stream.prefix.lock().unwrap().len(), PREFIX_BYTES);
        drop(server);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn interrupted_streams_never_publish_partial_audio_or_append_an_http_error_to_it() {
        let origin = Origin::new();
        let agent = ureq::Agent::new_with_defaults();
        let root = directory();
        let cache = AudioCache::at(Some(root.clone()));
        let server = Server::new(agent.clone(), Some(root.clone())).unwrap();
        let stream = origin.stream();
        stream.prepare(&agent).unwrap();
        origin.fail_after_prefix.store(true, Ordering::Relaxed);
        let url = server.register(stream.clone());
        let path = url.split_once("/stream/").unwrap().1;
        let mut socket = TcpStream::connect(server.address).unwrap();
        write!(
            socket,
            "GET /stream/{path} HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));
        assert_eq!(&response[split..], &origin.bytes[..PREFIX_BYTES]);
        assert!(stream.rejected.load(Ordering::Relaxed));
        assert!(cache.lookup(&stream.id).is_none());
        drop(server);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancellation_and_expiry_prevent_more_network_requests_even_with_a_cached_prefix() {
        let origin = Origin::new();
        let agent = ureq::Agent::new_with_defaults();
        let server = Server::new(agent.clone(), None).unwrap();
        let stream = origin.stream();
        stream.prepare(&agent).unwrap();
        let url = server.register(stream.clone());
        stream.cancelled.store(true, Ordering::Relaxed);
        assert!(stream.fetch(&agent, 0, 1).is_err());
        assert!(matches!(
            agent.get(&url).call(),
            Err(ureq::Error::StatusCode(410))
        ));
        assert_eq!(origin.requests.lock().unwrap().len(), 1);
        let mut expired = origin.stream();
        Arc::get_mut(&mut expired).unwrap().expires = Instant::now() - Duration::from_secs(1);
        assert!(expired.fetch(&agent, 0, 1).is_err());
    }
    #[test]
    fn registry_lifetime_follows_cached_descriptors_instead_of_evicting_reused_urls() {
        let origin = Origin::new();
        let agent = ureq::Agent::new_with_defaults();
        let server = Server::new(agent.clone(), None).unwrap();
        let current = origin.stream();
        let url = server.register(current.clone());
        for _ in 0..100 {
            server.register(origin.stream());
        }
        assert!(server.registry.lock().unwrap().entries.len() <= 2);
        let bytes = agent
            .get(&url)
            .header("Range", "bytes=0-1")
            .call()
            .unwrap()
            .body_mut()
            .read_to_vec()
            .unwrap();
        assert_eq!(bytes, origin.bytes[..2]);
    }
    #[test]
    fn output_backpressure_waits_for_resume_and_remains_cancellable() {
        for cancel in [false, true] {
            let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
            let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_write_timeout(Some(Duration::from_millis(10)))
                .unwrap();
            let stream = Arc::new(Stream::new(
                "test".into(),
                String::new(),
                String::new(),
                1,
                Instant::now() + Duration::from_secs(60),
                Arc::new(AtomicBool::new(false)),
            ));
            let writing = stream.clone();
            let (done, result) = mpsc::channel();
            let worker = thread::spawn(move || {
                let result = write_audio(&mut socket, &vec![7; 8 * 1024 * 1024], &writing);
                done.send(result).unwrap();
            });
            thread::sleep(Duration::from_millis(150));
            assert!(result.try_recv().is_err());
            if cancel {
                stream.cancelled.store(true, Ordering::Relaxed);
                assert!(
                    result
                        .recv_timeout(Duration::from_secs(1))
                        .unwrap()
                        .is_err()
                );
            } else {
                let mut bytes = Vec::new();
                client.read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, vec![7; 8 * 1024 * 1024]);
                assert!(result.recv_timeout(Duration::from_secs(1)).unwrap().is_ok());
            }
            worker.join().unwrap();
        }
    }
    #[test]
    fn ranges_support_seeking_and_reject_invalid_requests() {
        assert_eq!(parse_range(None, 100).unwrap(), (0, 99));
        assert_eq!(parse_range(Some("bytes=20-"), 100).unwrap(), (20, 99));
        assert_eq!(parse_range(Some("bytes=-10"), 100).unwrap(), (90, 99));
        assert_eq!(parse_range(Some("bytes=20-999"), 100).unwrap(), (20, 99));
        for range in ["bytes=100-", "bytes=30-20", "bytes=0-1,4-5", "bytes=-0"] {
            assert!(parse_range(Some(range), 100).is_err());
        }
    }
}

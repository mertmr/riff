//! mpv lives on a worker; its JSON IPC observations are the source of playback truth.
use crate::{audio_cache::AudioCache, model::Track};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

pub enum PlayerCommand {
    Play {
        track: Track,
        generation: u64,
        position: f64,
        paused: bool,
    },
    SetPaused(bool),
    SetVolume(u8),
    Seek(f64),
    Stop,
    Shutdown,
}
#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    Ready,
    Recovering,
    Paused(bool),
    Buffering(bool),
    Position(f64),
    Duration(f64),
    Seekable(bool),
    Volume(u8),
    Stopped,
    Ended,
    Error(String),
    ControlError(String),
}
pub struct PlayerEvent {
    pub generation: u64,
    pub event: PlaybackEvent,
}

pub fn start_player(
    wake: impl Fn() + Send + 'static,
) -> (
    Sender<PlayerCommand>,
    Receiver<PlayerEvent>,
    thread::JoinHandle<()>,
) {
    let (tx, rx) = mpsc::channel();
    let (events, event_rx) = mpsc::channel();
    let worker = thread::spawn(move || player_loop(rx, events, wake));
    (tx, event_rx, worker)
}

#[cfg(unix)]
type IpcStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type IpcStream = std::fs::File;

struct Connection {
    stream: IpcStream,
    pending: Vec<u8>,
}
impl Connection {
    fn open(path: &str) -> std::io::Result<Self> {
        #[cfg(unix)]
        let stream = {
            let s = IpcStream::connect(path)?;
            s.set_nonblocking(true)?;
            s
        };
        #[cfg(windows)]
        let stream = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        Ok(Self {
            stream,
            pending: Vec::new(),
        })
    }
    fn send(&mut self, value: Value) -> std::io::Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        self.stream.write_all(&bytes)
    }
    fn messages(&mut self) -> std::io::Result<Vec<Value>> {
        let mut buf = [0u8; 8192];
        loop {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::System::Pipes::PeekNamedPipe;
                let mut available = 0;
                // Peek avoids a blocking read on an idle named pipe.
                let ok = unsafe {
                    PeekNamedPipe(
                        self.stream.as_raw_handle(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    if !self.pending.is_empty() {
                        break;
                    }
                    return Err(std::io::Error::last_os_error());
                }
                if available == 0 {
                    break;
                }
            }
            match self.stream.read(&mut buf) {
                Ok(0) if !self.pending.is_empty() => break,
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "mpv disconnected",
                    ));
                }
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
            if self.pending.len() > 1024 * 1024 {
                return Err(std::io::Error::other("mpv response exceeded limit"));
            }
        }
        let mut out = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            if let Ok(value) = serde_json::from_slice(&self.pending[..end]) {
                out.push(value);
            }
            self.pending.drain(..=end);
        }
        Ok(out)
    }
}

// Audio has no need for mpv's video-sized, effectively whole-file cache.
const AUDIO_OPTIONS: &[&str] = &[
    "--no-config",
    "--no-video",
    "--force-window=no",
    "--no-terminal",
    "--ytdl=yes",
    "--ytdl-format=bestaudio/best",
    "--msg-level=all=error",
    "--network-timeout=20",
    "--demuxer-max-bytes=4MiB",
    "--demuxer-max-back-bytes=1MiB",
    "--cache-secs=30",
];

fn audio_command() -> Command {
    let mut command = Command::new("mpv");
    command.args(AUDIO_OPTIONS);
    // Apple's allocator otherwise retains hundreds of MiB of freed large blocks
    // in the streaming subprocess. This is local to mpv, not a system setting.
    // See apple-oss-distributions/libmalloc/src/malloc.c: MallocLargeCache.
    #[cfg(target_os = "macos")]
    command.env("MallocLargeCache", "0");
    command
}

const LOAD_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_RETRIES: u8 = 3;

struct ActivePlayback {
    track: Track,
    position: f64,
    paused: bool,
    retries: u8,
    retry_at: Option<Instant>,
    ready: bool,
    cached: bool,
    cache_started: bool,
    started: Instant,
    progress: Instant,
    buffering_since: Option<Instant>,
}

impl ActivePlayback {
    fn new(track: Track, position: f64, paused: bool) -> Self {
        Self {
            track,
            position: if position.is_finite() {
                position.max(0.)
            } else {
                0.
            },
            paused,
            retries: 0,
            retry_at: None,
            ready: false,
            cached: false,
            cache_started: false,
            started: Instant::now(),
            progress: Instant::now(),
            buffering_since: None,
        }
    }

    fn retry(&mut self, now: Instant) -> bool {
        if self.retries >= MAX_RETRIES {
            return false;
        }
        self.retry_at = Some(now + Duration::from_secs(1 << self.retries));
        self.retries += 1;
        self.ready = false;
        self.buffering_since = None;
        true
    }

    fn timed_out(&self) -> bool {
        (!self.ready && self.started.elapsed() > LOAD_TIMEOUT)
            || (self.ready && !self.paused && self.progress.elapsed() > LOAD_TIMEOUT)
            || (!self.paused
                && self
                    .buffering_since
                    .is_some_and(|t| t.elapsed() > LOAD_TIMEOUT))
    }
}

fn launch(
    active: &mut ActivePlayback,
    cache: &AudioCache,
    generation: u64,
    volume: u8,
    mut command: Command,
) -> std::io::Result<(Child, String)> {
    let path = ipc_path(generation);
    let cached = cache.lookup(&active.track.id);
    active.cached = cached.is_some();
    let source = cached.unwrap_or_else(|| active.track.youtube_url().into());
    let child = command
        .args([
            format!("--volume={volume}"),
            format!("--input-ipc-server={path}"),
            format!("--start={}", active.position),
            format!("--pause={}", if active.paused { "yes" } else { "no" }),
        ])
        .arg("--")
        .arg(source)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    active.started = Instant::now();
    active.progress = active.started;
    active.ready = false;
    active.retry_at = None;
    Ok((child, path))
}

fn player_loop(commands: Receiver<PlayerCommand>, events: Sender<PlayerEvent>, wake: impl Fn()) {
    player_loop_with(commands, events, wake, AudioCache::new(), audio_command);
}

fn player_loop_with(
    commands: Receiver<PlayerCommand>,
    events: Sender<PlayerEvent>,
    wake: impl Fn(),
    mut cache: AudioCache,
    make_command: impl Fn() -> Command,
) {
    let mut process: Option<Child> = None;
    let mut ipc = None;
    let mut connection: Option<Connection> = None;
    let mut active: Option<ActivePlayback> = None;
    let mut volume = 70;
    let mut generation = 0;
    let mut last_position = Instant::now() - Duration::from_secs(1);
    let mut emit = |generation, event| {
        if matches!(event, PlaybackEvent::Position(_)) {
            if last_position.elapsed() < Duration::from_millis(250) {
                return;
            }
            last_position = Instant::now();
        }
        if events.send(PlayerEvent { generation, event }).is_ok() {
            wake();
        }
    };
    loop {
        let command = if active.is_some() || cache.downloading() {
            commands.recv_timeout(Duration::from_millis(50))
        } else {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match command {
            Ok(PlayerCommand::Play {
                track,
                generation: next,
                position,
                paused,
            }) => {
                terminate(&mut process, &mut ipc);
                connection = None;
                cache.cancel();
                generation = next;
                active = Some(ActivePlayback::new(track, position, paused));
            }
            Ok(PlayerCommand::SetPaused(value)) => {
                if let Some(active) = &mut active {
                    active.paused = value;
                    active.progress = Instant::now();
                    // A long deliberate pause must not spend the stall timeout.
                    if active.buffering_since.is_some() {
                        active.buffering_since = Some(Instant::now());
                    }
                }
                if let Some(c) = &mut connection
                    && let Err(e) =
                        c.send(json!({"command":["set_property","pause",value],"request_id":100}))
                {
                    emit(generation, PlaybackEvent::ControlError(e.to_string()));
                }
            }
            Ok(PlayerCommand::SetVolume(value)) => {
                volume = value.min(100);
                if let Some(c) = &mut connection
                    && let Err(e) =
                        c.send(json!({"command":["set_property","volume",volume],"request_id":100}))
                {
                    emit(generation, PlaybackEvent::ControlError(e.to_string()));
                }
            }
            Ok(PlayerCommand::Seek(position)) => {
                if position.is_finite() {
                    if let Some(active) = &mut active {
                        active.position = position.max(0.);
                        active.progress = Instant::now();
                    }
                    if let Some(c) = &mut connection
                        && let Err(e) = c.send(json!({"command":["seek",position.max(0.0),"absolute+exact"],"request_id":100}))
                    { emit(generation, PlaybackEvent::ControlError(e.to_string())); }
                }
            }
            Ok(PlayerCommand::Stop) => {
                active = None;
                terminate(&mut process, &mut ipc);
                connection = None;
                cache.cancel();
                emit(generation, PlaybackEvent::Stopped);
            }
            Ok(PlayerCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                terminate(&mut process, &mut ipc);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        cache.poll();
        let Some(playback) = &mut active else {
            continue;
        };
        if process.is_none() && playback.retry_at.is_none_or(|at| Instant::now() >= at) {
            match launch(playback, &cache, generation, volume, make_command()) {
                Ok((child, path)) => {
                    process = Some(child);
                    ipc = Some(path);
                }
                Err(e) => {
                    emit(
                        generation,
                        PlaybackEvent::Error(format!(
                            "Couldn't start mpv: {e}. Install mpv and make sure it is on PATH."
                        )),
                    );
                    active = None;
                    cache.cancel();
                    continue;
                }
            }
        }
        let mut failure = None;
        let mut invalid_cache = false;
        if connection.is_none()
            && let Some(path) = &ipc
            && let Ok(mut c) = Connection::open(path)
        {
            // Commands can arrive between process launch and IPC connection.
            // Apply their latest values before subscribing to property snapshots.
            for value in [
                json!({"command":["set_property","volume",volume]}),
                json!({"command":["set_property","pause",playback.paused]}),
            ] {
                if let Err(e) = c.send(value) {
                    failure = Some(e.to_string());
                }
            }
            for (i, property) in [
                "time-pos",
                "duration",
                "pause",
                "paused-for-cache",
                "seekable",
                "volume",
            ]
            .iter()
            .enumerate()
            {
                if let Err(e) = c.send(json!({"command":["observe_property",i+1,property]})) {
                    failure = Some(e.to_string());
                    break;
                }
            }
            connection = Some(c);
        }
        let mut ended = false;
        if let Some(c) = &mut connection {
            match c.messages() {
                Ok(messages) => {
                    for message in messages {
                        if let Some(event) = parse_message(&message) {
                            // A local file can finish loading before IPC connects.
                            // Observed time-pos supplies the same readiness truth.
                            if !playback.ready
                                && matches!(
                                    event,
                                    PlaybackEvent::Ready | PlaybackEvent::Position(_)
                                )
                            {
                                playback.ready = true;
                                if !playback.cached && !playback.cache_started {
                                    cache.start(&playback.track);
                                    playback.cache_started = true;
                                }
                                emit(generation, PlaybackEvent::Ready);
                                emit(generation, PlaybackEvent::Paused(playback.paused));
                            }
                            match &event {
                                PlaybackEvent::Ready => continue,
                                PlaybackEvent::Position(p) => {
                                    if (*p - playback.position).abs() > 0.001 {
                                        playback.progress = Instant::now();
                                    }
                                    playback.position = *p;
                                }
                                PlaybackEvent::Buffering(value) => {
                                    if *value {
                                        playback.buffering_since.get_or_insert_with(Instant::now);
                                    } else {
                                        playback.buffering_since = None;
                                    }
                                }
                                PlaybackEvent::Error(e) => {
                                    invalid_cache = playback.cached;
                                    failure = Some(e.clone());
                                    break;
                                }
                                PlaybackEvent::Ended => {
                                    ended = true;
                                    break;
                                }
                                _ => {}
                            }
                            emit(generation, event);
                        }
                    }
                }
                Err(e) => failure = Some(format!("Lost contact with the player: {e}")),
            }
        }
        if process.is_some() && playback.timed_out() {
            invalid_cache = playback.cached && !playback.ready;
            failure = Some("Audio stopped responding. Check your connection and retry.".into());
        }
        if !ended
            && failure.is_none()
            && let Some(child) = &mut process
        {
            match child.try_wait() {
                Ok(Some(status)) if status.success() && playback.ready => ended = true,
                Ok(Some(_)) => {
                    invalid_cache = playback.cached && !playback.ready;
                    failure = Some("This track couldn't be played. Check your connection or update mpv and yt-dlp.".into());
                }
                Err(e) => failure = Some(e.to_string()),
                Ok(None) => {}
            }
        }
        if ended {
            terminate(&mut process, &mut ipc);
            connection = None;
            active = None;
            emit(generation, PlaybackEvent::Ended);
        } else if let Some(error) = failure {
            terminate(&mut process, &mut ipc);
            connection = None;
            if invalid_cache {
                cache.invalidate(&playback.track.id);
                playback.cache_started = false;
            }
            if playback.retry(Instant::now()) {
                emit(generation, PlaybackEvent::Recovering);
            } else {
                emit(
                    generation,
                    PlaybackEvent::Error(format!(
                        "{error} Automatic recovery failed after {MAX_RETRIES} retries. Retry playback to resume."
                    )),
                );
                active = None;
                cache.cancel();
            }
        }
    }
}

fn parse_message(value: &Value) -> Option<PlaybackEvent> {
    match value["event"].as_str() {
        Some("file-loaded") => Some(PlaybackEvent::Ready),
        Some("end-file") => match value["reason"].as_str() {
            Some("eof") => Some(PlaybackEvent::Ended),
            Some("error") => Some(PlaybackEvent::Error(format!(
                "This track couldn't be played ({}). Try another track or update yt-dlp.",
                value["file_error"].as_str().unwrap_or("unavailable audio")
            ))),
            _ => None,
        },
        Some("property-change") => {
            let data = &value["data"];
            match value["name"].as_str()? {
                "time-pos" => data
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.)
                    .map(PlaybackEvent::Position),
                "duration" => data
                    .as_f64()
                    .filter(|v| v.is_finite() && *v > 0.)
                    .map(PlaybackEvent::Duration),
                "pause" => data.as_bool().map(PlaybackEvent::Paused),
                "paused-for-cache" => data.as_bool().map(PlaybackEvent::Buffering),
                "seekable" => data.as_bool().map(PlaybackEvent::Seekable),
                "volume" => data
                    .as_f64()
                    .map(|v| PlaybackEvent::Volume(v.clamp(0., 100.) as u8)),
                _ => None,
            }
        }
        _ if value["request_id"] == 100
            && value["error"].as_str().is_some_and(|s| s != "success") =>
        {
            Some(PlaybackEvent::ControlError(format!(
                "Player control failed: {}",
                value["error"].as_str()?
            )))
        }
        _ => None,
    }
}

fn terminate(process: &mut Option<Child>, ipc: &mut Option<String>) {
    if let Some(mut child) = process.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    cleanup(ipc);
}
fn cleanup(ipc: &mut Option<String>) {
    if let Some(path) = ipc.take() {
        #[cfg(unix)]
        let _ = std::fs::remove_file(path);
        #[cfg(not(unix))]
        let _ = path;
    }
}
fn ipc_path(sequence: u64) -> String {
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\riff-{}-{sequence}", std::process::id())
    }
    #[cfg(not(windows))]
    {
        std::env::temp_dir()
            .join(format!("riff-{}-{sequence}.sock", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track() -> Track {
        Track {
            id: "dQw4w9WgXcQ".into(),
            title: "Test".into(),
            artist: "Test".into(),
            duration_seconds: None,
        }
    }
    #[test]
    fn recovery_is_bounded_preserves_position_and_pause_and_detects_stalls() {
        let mut active = ActivePlayback::new(track(), 17.5, true);
        let now = Instant::now();
        for delay in [1, 2, 4] {
            assert!(active.retry(now));
            assert_eq!(active.retry_at, Some(now + Duration::from_secs(delay)));
            assert_eq!(active.position, 17.5);
            assert!(active.paused);
        }
        assert!(!active.retry(now));
        active.ready = true;
        active.buffering_since = Some(now - LOAD_TIMEOUT - Duration::from_secs(1));
        assert!(!active.timed_out());
        active.paused = false;
        assert!(active.timed_out());
        active.buffering_since = None;
        assert!(!active.timed_out());
        active.progress = now - LOAD_TIMEOUT - Duration::from_secs(1);
        assert!(active.timed_out());
        active.paused = true;
        assert!(!active.timed_out());
        assert_eq!(ActivePlayback::new(track(), f64::NAN, false).position, 0.);
    }
    #[cfg(unix)]
    #[test]
    fn ipc_delivers_final_messages_before_reporting_disconnect() {
        let (stream, mut writer) = IpcStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        let mut connection = Connection {
            stream,
            pending: Vec::new(),
        };
        writer
            .write_all(b"{\"event\":\"end-file\",\"reason\":\"eof\"}\n")
            .unwrap();
        drop(writer);
        assert_eq!(connection.messages().unwrap().len(), 1);
        assert_eq!(
            connection.messages().unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
    #[cfg(unix)]
    #[test]
    fn worker_exhausts_retries_and_stop_cancels_pending_recovery() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        for stop in [false, true] {
            let launches = Arc::new(AtomicUsize::new(0));
            let count = launches.clone();
            let (commands, rx) = mpsc::channel();
            let (tx, events) = mpsc::channel();
            let worker = thread::spawn(move || {
                player_loop_with(
                    rx,
                    tx,
                    || {},
                    AudioCache::at(None),
                    move || {
                        count.fetch_add(1, Ordering::Relaxed);
                        let mut command = Command::new("/bin/sh");
                        command.args(["-c", "exit 1"]);
                        command
                    },
                )
            });
            commands
                .send(PlayerCommand::Play {
                    track: track(),
                    generation: 71,
                    position: 13.,
                    paused: true,
                })
                .unwrap();
            let result = std::panic::catch_unwind(|| {
                for attempt in 0..=MAX_RETRIES {
                    let event = events.recv_timeout(Duration::from_secs(6)).unwrap();
                    assert_eq!(event.generation, 71);
                    if attempt < MAX_RETRIES {
                        assert!(matches!(event.event, PlaybackEvent::Recovering));
                        if stop {
                            commands.send(PlayerCommand::Stop).unwrap();
                            assert!(matches!(
                                events.recv_timeout(Duration::from_secs(1)).unwrap().event,
                                PlaybackEvent::Stopped
                            ));
                            assert!(events.recv_timeout(Duration::from_millis(1200)).is_err());
                            assert_eq!(launches.load(Ordering::Relaxed), 1);
                            break;
                        }
                    } else {
                        assert!(matches!(event.event, PlaybackEvent::Error(_)));
                        assert_eq!(launches.load(Ordering::Relaxed), 4);
                        assert!(events.try_recv().is_err());
                    }
                }
            });
            commands.send(PlayerCommand::Shutdown).unwrap();
            worker.join().unwrap();
            if let Err(e) = result {
                std::panic::resume_unwind(e);
            }
        }
    }
    #[test]
    fn idle_worker_wakes_for_shutdown_and_channel_disconnect() {
        for shutdown in [true, false] {
            let (commands, _events, worker) = start_player(|| {});
            commands.send(PlayerCommand::SetVolume(37)).unwrap();
            commands.send(PlayerCommand::Stop).unwrap();
            if shutdown {
                commands.send(PlayerCommand::Shutdown).unwrap();
            }
            drop(commands);
            let (done, finished) = mpsc::channel();
            thread::spawn(move || done.send(worker.join()).unwrap());
            finished
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();
        }
    }
    #[test]
    fn playback_truth_comes_from_ipc() {
        assert!(matches!(
            parse_message(&json!({"event":"file-loaded"})),
            Some(PlaybackEvent::Ready)
        ));
        assert!(
            matches!(parse_message(&json!({"event":"property-change","name":"time-pos","data":12.5})),Some(PlaybackEvent::Position(p)) if p == 12.5)
        );
        assert!(
            parse_message(&json!({"event":"property-change","name":"time-pos","data":null}))
                .is_none()
        );
        assert!(matches!(
            parse_message(
                &json!({"event":"end-file","reason":"error","file_error":"loading failed"})
            ),
            Some(PlaybackEvent::Error(_))
        ));
        assert!(parse_message(&json!({"event":"end-file","reason":"stop"})).is_none());
    }
}

#[cfg(all(test, unix))]
mod integration_tests {
    use super::*;
    fn property(c: &mut Connection, name: &str) -> Value {
        c.send(json!({"command":["get_property",name],"request_id":777}))
            .unwrap();
        let start = Instant::now();
        loop {
            for value in c.messages().unwrap() {
                if value["request_id"] == 777 {
                    assert_eq!(value["error"], "success");
                    return value["data"].clone();
                }
            }
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn write_wav(path: &std::path::Path) {
        let sample_rate = 8000u32;
        let size = sample_rate * 20 * 2;
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((size + 36).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(1u16.to_le_bytes());
        bytes.extend(sample_rate.to_le_bytes());
        bytes.extend((sample_rate * 2).to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(size.to_le_bytes());
        bytes.resize(size as usize + 44, 0);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    #[ignore = "requires mpv on PATH; tests offline caching and recovery from a real player crash"]
    fn cached_audio_recovers_offline_at_the_same_position_and_pause_state() {
        let dir = std::env::temp_dir().join(format!("riff-offline-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = Track {
            id: "dQw4w9WgXcQ".into(),
            title: "Offline".into(),
            artist: "Test".into(),
            duration_seconds: Some(20),
        };
        let cached = dir.join(format!("{}.audio", track.id));
        write_wav(&cached);
        let cache = AudioCache::at(Some(dir.clone()));
        let (commands, rx) = mpsc::channel();
        let (tx, events) = mpsc::channel();
        let worker = thread::spawn(move || {
            player_loop_with(
                rx,
                tx,
                || {},
                cache,
                || {
                    let mut command = audio_command();
                    command.args(["--ao=null", "--ytdl=no"]);
                    command
                },
            )
        });
        let result = std::panic::catch_unwind(|| {
            fn wait(events: &Receiver<PlayerEvent>, predicate: impl Fn(&PlaybackEvent) -> bool) {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    let event = events
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .unwrap();
                    assert_eq!(event.generation, 888_888);
                    assert!(
                        !matches!(event.event, PlaybackEvent::Error(_)),
                        "{:?}",
                        event.event
                    );
                    if predicate(&event.event) {
                        return;
                    }
                }
            }
            commands.send(PlayerCommand::SetVolume(37)).unwrap();
            commands
                .send(PlayerCommand::Play {
                    track: track.clone(),
                    generation: 888_888,
                    position: 7.5,
                    paused: true,
                })
                .unwrap();
            wait(&events, |e| matches!(e, PlaybackEvent::Ready));
            wait(
                &events,
                |e| matches!(e, PlaybackEvent::Position(p) if (*p - 7.5).abs() < 0.1),
            );
            let mut ipc = Connection::open(&ipc_path(888_888)).unwrap();
            assert_eq!(property(&mut ipc, "pause"), true);
            assert_eq!(property(&mut ipc, "volume"), 37.);
            let pid = property(&mut ipc, "pid").as_u64().unwrap();
            assert!(
                Command::new("kill")
                    .args(["-KILL", &pid.to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            wait(&events, |e| matches!(e, PlaybackEvent::Recovering));
            assert!(
                cached.exists(),
                "A player crash must not invalidate healthy cached audio"
            );
            commands.send(PlayerCommand::SetVolume(42)).unwrap();
            wait(&events, |e| matches!(e, PlaybackEvent::Ready));
            wait(
                &events,
                |e| matches!(e, PlaybackEvent::Position(p) if (*p - 7.5).abs() < 0.1),
            );
            let mut ipc = Connection::open(&ipc_path(888_888)).unwrap();
            assert_eq!(property(&mut ipc, "pause"), true);
            assert_eq!(property(&mut ipc, "volume"), 42.);
            commands.send(PlayerCommand::Stop).unwrap();
            wait(&events, |e| matches!(e, PlaybackEvent::Stopped));
            // A decoder failure must evict the broken file before trying the network.
            std::fs::write(&cached, b"broken audio").unwrap();
            commands
                .send(PlayerCommand::Play {
                    track,
                    generation: 888_888,
                    position: 0.,
                    paused: false,
                })
                .unwrap();
            wait(&events, |e| matches!(e, PlaybackEvent::Recovering));
            assert!(!cached.exists());
            commands.send(PlayerCommand::Stop).unwrap();
            wait(&events, |e| matches!(e, PlaybackEvent::Stopped));
        });
        let _ = commands.send(PlayerCommand::Shutdown);
        worker.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
    #[test]
    #[ignore = "requires mpv on PATH; exercises real JSON IPC with local audio"]
    fn mpv_pause_volume_and_seek() {
        let path = ipc_path(999_999);
        let wav = std::env::temp_dir().join(format!("riff-ipc-test-{}.wav", std::process::id()));
        write_wav(&wav);
        let child = audio_command()
            .args([
                "--ao=null",
                "--pause=yes",
                "--idle=yes",
                &format!("--input-ipc-server={path}"),
            ])
            .arg(&wav)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("mpv must be installed");
        let mut process = Some(child);
        let mut ipc = Some(path.clone());
        let result = std::panic::catch_unwind(|| {
            let start = Instant::now();
            let mut c = loop {
                if let Ok(c) = Connection::open(&path) {
                    break c;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(5),
                    "mpv IPC did not appear"
                );
                thread::sleep(Duration::from_millis(30));
            };
            assert_eq!(property(&mut c, "pause"), true);
            c.send(json!({"command":["set_property","volume",37]}))
                .unwrap();
            assert_eq!(property(&mut c, "volume"), 37.);
            assert_eq!(property(&mut c, "seekable"), true);
            c.send(json!({"command":["seek",7.5,"absolute+exact"]}))
                .unwrap();
            thread::sleep(Duration::from_millis(100));
            assert!((property(&mut c, "time-pos").as_f64().unwrap() - 7.5).abs() < 0.3);
            c.send(json!({"command":["set_property","pause",false]}))
                .unwrap();
            assert_eq!(property(&mut c, "pause"), false);
        });
        terminate(&mut process, &mut ipc);
        let _ = std::fs::remove_file(wav);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}

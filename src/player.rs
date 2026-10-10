//! mpv lives on a worker; its JSON IPC observations are the source of playback truth.
use crate::model::Track;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
mod process;
mod resolver;
use process::ChildProcess as Child;
use resolver::{Media, Resolver};

pub enum PlayerCommand {
    Play {
        track: Track,
        generation: u64,
    },
    PrepareNext {
        track: Option<Track>,
        generation: u64,
        revision: u64,
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
    Transition { track: Track, revision: u64 },
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
    outgoing: Vec<u8>,
    written: usize,
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
        let stream = {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Pipes::{
                PIPE_NOWAIT, PIPE_READMODE_BYTE, SetNamedPipeHandleState,
            };
            let stream = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)?;
            let mode = PIPE_NOWAIT | PIPE_READMODE_BYTE;
            if unsafe {
                SetNamedPipeHandleState(
                    stream.as_raw_handle(),
                    &mode,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            stream
        };
        Ok(Self {
            stream,
            pending: Vec::new(),
            outgoing: Vec::new(),
            written: 0,
        })
    }
    fn send(&mut self, value: Value) -> std::io::Result<()> {
        if self.outgoing.len() > 256 * 1024 {
            return Err(std::io::Error::other("mpv command buffer exceeded limit"));
        }
        serde_json::to_writer(&mut self.outgoing, &value)?;
        self.outgoing.push(b'\n');
        self.flush()
    }
    fn flush(&mut self) -> std::io::Result<()> {
        while self.written < self.outgoing.len() {
            match self.stream.write(&self.outgoing[self.written..]) {
                Ok(0) => {
                    // A nonblocking Windows byte pipe can accept zero bytes
                    // when full; keep the unwritten command for the next poll.
                    #[cfg(windows)]
                    return Ok(());
                    #[cfg(not(windows))]
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                Ok(n) => self.written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        self.outgoing.clear();
        self.written = 0;
        Ok(())
    }
    fn messages(&mut self) -> std::io::Result<Vec<Value>> {
        self.flush()?;
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
                    return Err(std::io::Error::last_os_error());
                }
                if available == 0 {
                    break;
                }
            }
            match self.stream.read(&mut buf) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            if self.pending.len() > 1024 * 1024 {
                return Err(std::io::Error::other("mpv response exceeded limit"));
            }
        }
        let mut out = Vec::new();
        let mut consumed = 0;
        for end in self
            .pending
            .iter()
            .enumerate()
            .filter_map(|(i, b)| (*b == b'\n').then_some(i))
        {
            if let Ok(value) = serde_json::from_slice(&self.pending[consumed..end]) {
                out.push(value);
            }
            consumed = end + 1;
        }
        self.pending.drain(..consumed);
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
    "--idle=yes",
    "--gapless-audio=yes",
    "--audio-samplerate=48000",
    "--audio-channels=stereo",
    "--prefetch-playlist=yes",
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

// Playlist IDs, rather than delivery time, identify which track an IPC event
// belongs to. Replacement loads and queue edits may race an automatic handoff.
#[derive(Clone)]
struct Binding {
    track: Track,
    generation: u64,
    epoch: u64,
    transition: Option<(u64, u64)>, // parent generation, queue revision
}
struct Playing {
    binding: Binding,
    entry: Option<i64>,
    ready: bool,
    ended: bool,
    awaiting_next: bool,
    position: f64,
    duration: Option<f64>,
    seekable: bool,
    retries: u8,
    started: Instant,
    last_progress: Instant,
    buffering_since: Option<Instant>,
    buffering: bool,
    retry_at: Option<Instant>,
}
struct Upcoming {
    binding: Binding,
    media: Option<Media>,
    entry: Option<i64>,
    attempted: bool,
}
struct Handoff {
    previous: Playing,
    revision: u64,
    audio_pts: Option<f64>,
}
struct Engine<W: Fn()> {
    process: Option<Child>,
    ipc: Option<String>,
    connection: Option<Connection>,
    current: Option<Playing>,
    next: Option<Upcoming>,
    handoff: Option<Handoff>,
    resolver: Option<Resolver>,
    bindings: HashMap<i64, Binding>,
    requests: HashMap<u64, Binding>,
    messages: VecDeque<Value>,
    active_entry: Option<i64>,
    epoch: u64,
    request_id: u64,
    last_heartbeat: Instant,
    heartbeat_sent: Option<Instant>,
    volume: u8,
    paused: bool,
    last_position: Instant,
    events: Sender<PlayerEvent>,
    wake: W,
    source: fn(&Track) -> String,
    command: fn() -> Command,
    #[cfg(test)]
    extra_args: Vec<String>,
}
impl<W: Fn()> Engine<W> {
    fn new(events: Sender<PlayerEvent>, wake: W) -> Self {
        Self {
            process: None,
            ipc: None,
            connection: None,
            current: None,
            next: None,
            handoff: None,
            resolver: None,
            bindings: HashMap::new(),
            requests: HashMap::new(),
            messages: VecDeque::new(),
            active_entry: None,
            epoch: 0,
            request_id: 1000,
            last_heartbeat: Instant::now(),
            heartbeat_sent: None,
            volume: 70,
            paused: false,
            last_position: Instant::now() - Duration::from_secs(1),
            events,
            wake,
            source: Track::youtube_url,
            command: audio_command,
            #[cfg(test)]
            extra_args: Vec::new(),
        }
    }
    fn emit(&mut self, generation: u64, event: PlaybackEvent) {
        let generation = if let Some(handoff) = &self.handoff {
            if generation == self.generation() {
                // Decoder metadata belongs to the next file while the output
                // device is still playing the previous file's buffered tail.
                if matches!(
                    event,
                    PlaybackEvent::Paused(_)
                        | PlaybackEvent::Volume(_)
                        | PlaybackEvent::ControlError(_)
                ) {
                    handoff.previous.binding.generation
                } else {
                    return;
                }
            } else {
                generation
            }
        } else {
            generation
        };
        if matches!(event, PlaybackEvent::Position(_)) {
            if self.last_position.elapsed() < Duration::from_millis(250) {
                return;
            }
            self.last_position = Instant::now();
        }
        if self.events.send(PlayerEvent { generation, event }).is_ok() {
            (self.wake)();
        }
    }
    fn generation(&self) -> u64 {
        self.current.as_ref().map_or(0, |c| c.binding.generation)
    }
    fn publish_handoff(&mut self) {
        let Some(handoff) = self.handoff.take() else {
            return;
        };
        let Some(current) = &self.current else { return };
        let generation = current.binding.generation;
        let track = current.binding.track.clone();
        let (ready, position, duration, seekable, buffering) = (
            current.ready,
            current.position,
            current.duration,
            current.seekable,
            current.buffering,
        );
        self.last_position = Instant::now() - Duration::from_secs(1);
        self.emit(
            handoff.previous.binding.generation,
            PlaybackEvent::Transition {
                track,
                revision: handoff.revision,
            },
        );
        if ready {
            self.emit(generation, PlaybackEvent::Ready);
        }
        if let Some(duration) = duration {
            self.emit(generation, PlaybackEvent::Duration(duration));
        }
        self.emit(generation, PlaybackEvent::Seekable(seekable));
        self.emit(generation, PlaybackEvent::Position(position));
        self.emit(generation, PlaybackEvent::Buffering(buffering));
        self.emit(generation, PlaybackEvent::Paused(self.paused));
    }
    fn audio_clock(&mut self, pts: f64) {
        if !pts.is_finite() || self.handoff.is_none() {
            return;
        }
        if pts >= 0. {
            if let Some(current) = &mut self.current {
                current.position = pts;
            }
            self.publish_handoff();
        } else if let Some(handoff) = &mut self.handoff {
            if let Some(previous_pts) = handoff.audio_pts {
                handoff.previous.position += (pts - previous_pts).max(0.);
            }
            handoff.audio_pts = Some(pts);
            let generation = handoff.previous.binding.generation;
            let position = handoff.previous.position;
            self.emit(generation, PlaybackEvent::Position(position));
        }
    }
    fn restore_audible_track(&mut self, position: Option<f64>) -> bool {
        let Some(handoff) = self.handoff.take() else {
            return false;
        };
        let Some(decoded) = self.current.take() else {
            return false;
        };
        self.epoch = self.epoch.wrapping_add(1);
        self.resolver = None;
        self.bindings.clear();
        self.requests.clear();
        self.messages.clear();
        self.active_entry = None;
        let mut previous = handoff.previous;
        previous.binding.epoch = self.epoch;
        previous.entry = None;
        previous.ready = false;
        previous.awaiting_next = false;
        previous.started = Instant::now();
        previous.last_progress = Instant::now();
        if let Some(position) = position {
            previous.position = position.max(0.);
        }
        let parent = previous.binding.generation;
        let mut binding = decoded.binding;
        binding.epoch = self.epoch;
        binding.transition = Some((parent, handoff.revision));
        self.next = Some(Upcoming {
            binding,
            media: None,
            entry: None,
            attempted: false,
        });
        self.current = Some(previous);
        true
    }
    fn send(&mut self, command: Value) -> std::io::Result<()> {
        if let Some(connection) = &mut self.connection {
            connection.send(command)
        } else {
            Ok(())
        }
    }
    fn disconnect(&mut self) {
        self.connection = None;
        terminate(&mut self.process, &mut self.ipc);
        self.bindings.clear();
        self.requests.clear();
        self.messages.clear();
        self.active_entry = None;
        self.heartbeat_sent = None;
    }
    fn clear_next(&mut self) {
        self.resolver = None; // Drop kills and reaps cancelled extraction immediately.
        self.next = None;
        if let Err(error) = self.send(json!({"command":["playlist-clear"],"request_id":100})) {
            self.fail(format!("Lost contact with the player: {error}"));
        }
    }
    fn control(&mut self, command: Value) {
        if let Err(error) = self.send(json!({"command":command,"request_id":100})) {
            self.fail(format!("Lost contact with the player: {error}"));
        }
    }
    fn handle(&mut self, command: PlayerCommand) -> bool {
        match command {
            PlayerCommand::Play { track, generation } => {
                self.handoff = None;
                self.clear_next();
                self.epoch = self.epoch.wrapping_add(1);
                self.requests.clear();
                self.bindings.clear();
                self.messages.clear();
                self.active_entry = None;
                self.last_position = Instant::now() - Duration::from_secs(1);
                self.paused = false;
                self.current = Some(Playing {
                    binding: Binding {
                        track,
                        generation,
                        epoch: self.epoch,
                        transition: None,
                    },
                    entry: None,
                    ready: false,
                    ended: false,
                    awaiting_next: false,
                    position: 0.,
                    duration: None,
                    seekable: false,
                    retries: 0,
                    started: Instant::now(),
                    last_progress: Instant::now(),
                    buffering_since: None,
                    buffering: false,
                    retry_at: None,
                });
                self.control(json!(["set_property", "pause", false]));
                if self.connection.is_some() {
                    self.load_current();
                }
            }
            PlayerCommand::PrepareNext {
                track,
                generation,
                revision,
            } => {
                if generation != self.generation() || self.current.as_ref().is_none_or(|c| c.ended)
                {
                    return true;
                }
                // Ignore identical scheduling requests, but honour revisions even
                // for duplicate tracks: a queue occurrence must only be consumed once.
                if self.next.as_ref().is_some_and(|n| {
                    n.binding.transition == Some((generation, revision))
                        && track.as_ref() == Some(&n.binding.track)
                }) {
                    return true;
                }
                self.clear_next();
                self.next = track.map(|track| Upcoming {
                    binding: Binding {
                        track,
                        generation: generation.wrapping_add(1),
                        epoch: self.epoch,
                        transition: Some((generation, revision)),
                    },
                    media: None,
                    entry: None,
                    attempted: false,
                });
            }
            PlayerCommand::SetPaused(value) => {
                self.paused = value;
                if let Some(current) = &mut self.current {
                    current.buffering_since = (current.buffering && !value).then(Instant::now);
                    current.last_progress = Instant::now();
                }
                self.control(json!(["set_property", "pause", value]));
            }
            PlayerCommand::SetVolume(value) => {
                self.volume = value.min(100);
                self.control(json!(["set_property", "volume", self.volume]));
            }
            PlayerCommand::Seek(position) if position.is_finite() => {
                if self.restore_audible_track(Some(position)) {
                    self.load_current();
                    return true;
                }
                if let Some(current) = &mut self.current {
                    current.position = position.max(0.);
                    current.last_progress = Instant::now();
                    current.buffering_since =
                        (current.buffering && !self.paused).then(Instant::now);
                }
                self.control(json!(["seek", position.max(0.), "absolute+exact"]));
            }
            PlayerCommand::Seek(_) => {}
            PlayerCommand::Stop | PlayerCommand::Shutdown => {
                let shutdown = matches!(command, PlayerCommand::Shutdown);
                let generation = self.generation();
                self.resolver = None;
                self.next = None;
                self.current = None;
                self.handoff = None;
                self.disconnect();
                if shutdown {
                    return false;
                }
                self.emit(generation, PlaybackEvent::Stopped);
            }
        }
        true
    }
    fn spawn(&mut self) {
        if self.process.is_some() || self.current.as_ref().is_none_or(|c| c.ended) {
            return;
        }
        static SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1_000_000);
        let path = ipc_path(SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        let mut command = (self.command)();
        #[cfg(test)]
        command.args(&self.extra_args);
        match Child::spawn(
            command
                .arg(format!("--volume={}", self.volume))
                .arg(format!(
                    "--pause={}",
                    if self.paused { "yes" } else { "no" }
                ))
                .arg(format!("--input-ipc-server={path}"))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        ) {
            Ok(child) => {
                self.process = Some(child);
                self.ipc = Some(path);
            }
            Err(error) => {
                if error.kind() == std::io::ErrorKind::NotFound {
                    self.finish_error(
                        "Couldn't start mpv. Install mpv and make sure it is on PATH.".into(),
                    );
                } else {
                    self.fail(format!("Couldn't start mpv: {error}"));
                }
            }
        }
    }
    fn load(&mut self, binding: Binding, media: Media, append: bool) {
        self.request_id += 1;
        let id = self.request_id;
        self.requests.insert(id, binding);
        let command = json!({"command":["loadfile", media.url, if append { "append" } else { "replace" }, -1, media.options],"request_id":id});
        if let Err(error) = self.send(command) {
            self.fail(format!("Couldn't load audio: {error}"));
        }
    }
    fn load_current(&mut self) {
        let Some(current) = &self.current else { return };
        let binding = current.binding.clone();
        let mut media = Media::plain((self.source)(&binding.track));
        if current.seekable && current.position > 0. {
            media.options["start"] = json!(current.position.to_string());
        }
        self.load(binding, media, false);
    }
    fn valid(&self, binding: &Binding) -> bool {
        if binding.epoch != self.epoch {
            return false;
        }
        if binding.transition.is_none() {
            self.current
                .as_ref()
                .is_some_and(|c| c.binding.generation == binding.generation)
        } else {
            self.next.as_ref().is_some_and(|n| {
                n.binding.generation == binding.generation
                    && n.binding.transition == binding.transition
            })
        }
    }
    fn prepare(&mut self) {
        if self
            .current
            .as_ref()
            .is_none_or(|c| !c.ready || c.ended || c.retry_at.is_some())
        {
            return;
        }
        if let Some(result) = self.resolver.as_mut().and_then(Resolver::poll) {
            self.resolver = None;
            if let (Ok(media), Some(next)) = (result, self.next.as_mut()) {
                next.media = Some(media);
            }
            // A failed preparation never interrupts the current track. Normal
            // queue advancement uses mpv's extractor as a fresh fallback.
        }
        let Some(next) = &mut self.next else { return };
        if !next.attempted {
            next.attempted = true;
            let url = (self.source)(&next.binding.track);
            if url.starts_with("https://www.youtube.com/watch?") {
                self.resolver = Resolver::start(&url).ok();
            } else {
                next.media = Some(Media::plain(url));
            }
        }
        if let Some(media) = next.media.take() {
            let binding = next.binding.clone();
            self.load(binding, media, true);
        }
    }
    fn finish_error(&mut self, error: String) {
        self.restore_audible_track(None);
        let generation = self.generation();
        if let Some(current) = &mut self.current {
            if current.ended {
                return;
            }
            current.ended = true;
            current.retry_at = None;
        }
        self.resolver = None;
        self.next = None;
        self.disconnect();
        self.emit(
            generation,
            PlaybackEvent::Error(format!("{error} Retry or choose another track.")),
        );
    }
    fn fail(&mut self, error: String) {
        self.restore_audible_track(None);
        let Some(current) = &mut self.current else {
            self.disconnect();
            return;
        };
        if current.ended || current.retry_at.is_some() {
            return;
        }
        if current.retries >= 2 {
            self.finish_error(error);
            return;
        }
        current.retries += 1;
        current.ready = false;
        current.awaiting_next = false;
        current.entry = None;
        current.buffering_since = None;
        current.buffering = false;
        current.retry_at =
            Some(Instant::now() + Duration::from_millis(500 * u64::from(current.retries)));
        self.epoch = self.epoch.wrapping_add(1);
        current.binding.epoch = self.epoch;
        self.resolver = None;
        if let Some(next) = &mut self.next {
            next.binding.epoch = self.epoch;
            next.entry = None;
            next.media = None;
            next.attempted = false;
        }
        self.disconnect();
        self.emit(self.generation(), PlaybackEvent::Recovering);
    }
    fn accept_reply(&mut self, message: &Value) {
        let Some(id) = message["request_id"].as_u64() else {
            return;
        };
        let Some(binding) = self.requests.remove(&id) else {
            return;
        };
        if !self.valid(&binding) {
            return;
        }
        let Some(entry) = message["data"]["playlist_entry_id"].as_i64() else {
            if binding.transition.is_none() {
                self.fail(format!(
                    "mpv couldn't load this track ({})",
                    message["error"]
                        .as_str()
                        .unwrap_or("missing playlist ID; update mpv")
                ));
            } else {
                // Preparation is optional; EOF advances through the normal
                // queue path without surfacing a background extraction failure.
                self.next = None;
            }
            return;
        };
        // Keep only the current and prepared entry. Removed-entry events are stale.
        let current_entry = self.current.as_ref().and_then(|c| c.entry);
        self.bindings.retain(|id, _| Some(*id) == current_entry);
        if binding.transition.is_some() {
            if let Some(next) = &mut self.next {
                next.entry = Some(entry);
            }
        } else if let Some(current) = &mut self.current {
            current.entry = Some(entry);
        }
        self.bindings.insert(entry, binding);
    }
    fn message(&mut self, message: Value) {
        if message["event"] == "start-file" {
            self.active_entry = message["playlist_entry_id"].as_i64();
            let binding = self
                .active_entry
                .and_then(|id| self.bindings.get(&id))
                .cloned();
            if binding.as_ref().is_none_or(|binding| !self.valid(binding))
                && self.current.as_ref().is_some_and(|c| c.awaiting_next)
            {
                self.control(json!(["stop"]));
                self.end();
                return;
            }
            let Some(binding) = binding else { return };
            if !self.valid(&binding) {
                return;
            }
            if let Some((parent, revision)) = binding.transition {
                if parent != self.generation() {
                    return;
                }
                self.resolver = None;
                self.next = None;
                let mut binding = binding;
                binding.transition = None;
                let previous = self.current.take().unwrap();
                self.current = Some(Playing {
                    binding,
                    entry: self.active_entry,
                    ready: false,
                    ended: false,
                    awaiting_next: false,
                    position: 0.,
                    duration: None,
                    seekable: false,
                    retries: 0,
                    started: Instant::now(),
                    last_progress: Instant::now(),
                    buffering_since: None,
                    buffering: false,
                    retry_at: None,
                });
                self.last_position = Instant::now() - Duration::from_secs(1);
                self.handoff = Some(Handoff {
                    previous,
                    revision,
                    audio_pts: None,
                });
            }
            return;
        }
        if message["request_id"] == 100 {
            if let Some(event) = parse_message(&message) {
                self.emit(self.generation(), event);
            }
            return;
        }
        if message["event"] == "property-change" && message["name"] == "idle-active" {
            if message["data"] == true && self.current.as_ref().is_some_and(|c| c.awaiting_next) {
                // mpv enters idle only after draining the last file's output.
                self.publish_handoff();
                self.end();
            }
            return;
        }
        let Some(current) = &self.current else { return };
        if current.retry_at.is_some() || current.ended {
            return;
        }
        if message["event"] == "end-file" {
            if message["playlist_entry_id"].as_i64() != current.entry {
                return;
            }
        } else if self.active_entry != current.entry || current.entry.is_none() {
            return;
        }
        if message["event"] == "property-change" && message["name"] == "audio-pts" {
            if let Some(pts) = message["data"].as_f64() {
                self.audio_clock(pts);
            }
            return;
        }
        let Some(event) = parse_message(&message) else {
            return;
        };
        match event {
            PlaybackEvent::Error(error) => self.fail(error),
            PlaybackEvent::Ended => {
                let current = self.current.as_ref().unwrap();
                if premature_eof(
                    current.ready,
                    current.seekable,
                    current.position,
                    current.duration,
                ) {
                    self.fail("The audio connection ended before the track finished.".into());
                } else {
                    // Decoder EOF precedes audible EOF. Wait for the next
                    // audio clock, or idle after mpv drains the final output.
                    if let Some(current) = &mut self.current {
                        current.started = Instant::now();
                        current.ready = false;
                        current.awaiting_next = true;
                    }
                }
            }
            PlaybackEvent::Ready => {
                if let Some(current) = &mut self.current {
                    current.buffering = false;
                    current.ready = true;
                    current.last_progress = Instant::now();
                    current.buffering_since = None;
                }
                self.emit(self.generation(), PlaybackEvent::Ready);
                // Explicitly report pause after Ready, even if the property didn't
                // change between consecutive tracks or a recovery attempt.
                self.emit(self.generation(), PlaybackEvent::Paused(self.paused));
            }
            PlaybackEvent::Position(position) => {
                if let Some(current) = &mut self.current {
                    if (current.position - position).abs() > 0.001 {
                        current.last_progress = Instant::now();
                    }
                    current.position = position;
                }
                self.emit(self.generation(), PlaybackEvent::Position(position));
            }
            PlaybackEvent::Duration(duration) => {
                if let Some(current) = &mut self.current {
                    current.duration = Some(duration);
                }
                self.emit(self.generation(), PlaybackEvent::Duration(duration));
            }
            PlaybackEvent::Seekable(seekable) => {
                if let Some(current) = &mut self.current {
                    current.seekable = seekable;
                }
                self.emit(self.generation(), PlaybackEvent::Seekable(seekable));
            }
            PlaybackEvent::Buffering(buffering) => {
                if let Some(current) = &mut self.current {
                    current.buffering = buffering;
                    if buffering && !self.paused {
                        current.buffering_since.get_or_insert_with(Instant::now);
                    } else {
                        current.buffering_since = None;
                    }
                }
                self.emit(self.generation(), PlaybackEvent::Buffering(buffering));
            }
            PlaybackEvent::Paused(paused) => {
                self.paused = paused;
                if let Some(current) = &mut self.current {
                    if current.buffering && !paused {
                        current.buffering_since.get_or_insert_with(Instant::now);
                    } else {
                        current.buffering_since = None;
                    }
                }
                self.emit(self.generation(), PlaybackEvent::Paused(paused));
            }
            other => self.emit(self.generation(), other),
        }
    }
    fn tick(&mut self) {
        if let Some(current) = &mut self.current
            && let Some(retry_at) = current.retry_at
        {
            if Instant::now() < retry_at {
                return;
            }
            current.retry_at = None;
            current.started = Instant::now();
        }
        self.spawn();
        if self.connection.is_none()
            && let Some(path) = &self.ipc
            && let Ok(connection) = Connection::open(path)
        {
            self.connection = Some(connection);
            self.last_heartbeat = Instant::now();
            for (i, property) in [
                "time-pos",
                "duration",
                "pause",
                "paused-for-cache",
                "seekable",
                "volume",
                "audio-pts",
                "idle-active",
            ]
            .iter()
            .enumerate()
            {
                self.control(json!(["observe_property", i + 1, property]));
            }
            self.control(json!(["set_property", "volume", self.volume]));
            self.control(json!(["set_property", "pause", self.paused]));
            if self.connection.is_some() {
                self.load_current();
            }
        }
        if let Some(connection) = &mut self.connection {
            match connection.messages() {
                Ok(messages) => {
                    // Replies can arrive after start-file. Bind IDs before replaying
                    // that batch; hold events if an earlier load reply is still pending.
                    for message in &messages {
                        if message["request_id"] == 101 {
                            self.heartbeat_sent = None;
                        }
                        self.accept_reply(message);
                    }
                    self.messages.extend(
                        messages
                            .into_iter()
                            .filter(|m| m.get("request_id").is_none() || m["request_id"] == 100),
                    );
                    if self.messages.len() > 4096 {
                        self.fail("The player stopped responding.".into());
                    }
                    while let Some(front) = self.messages.front() {
                        if front["event"] == "start-file"
                            && front["playlist_entry_id"]
                                .as_i64()
                                .is_some_and(|id| !self.bindings.contains_key(&id))
                            && !self.requests.is_empty()
                        {
                            break;
                        }
                        let message = self.messages.pop_front().unwrap();
                        self.message(message);
                    }
                }
                Err(error) => self.fail(format!("Lost contact with the player: {error}")),
            }
        }
        if self.connection.is_some() {
            if self
                .heartbeat_sent
                .is_some_and(|sent| sent.elapsed() > Duration::from_secs(10))
            {
                self.fail("The audio player stopped responding.".into());
            } else if self.heartbeat_sent.is_none()
                && self.last_heartbeat.elapsed() > Duration::from_secs(5)
            {
                self.last_heartbeat = Instant::now();
                self.heartbeat_sent = Some(Instant::now());
                if let Err(error) =
                    self.send(json!({"command":["get_property","pause"],"request_id":101}))
                {
                    self.fail(format!("Lost contact with the player: {error}"));
                }
            }
        }
        if let Some(child) = &mut self.process {
            match child.try_wait() {
                Ok(Some(_)) => self.fail("The audio player exited unexpectedly.".into()),
                Err(error) => self.fail(format!("Couldn't check the audio player: {error}")),
                Ok(None) => {}
            }
        }
        if let Some(current) = &self.current {
            if !current.ended && current.retry_at.is_none() {
                if !current.ready && current.started.elapsed() > Duration::from_secs(45) {
                    self.fail("This track took too long to load. Check your connection.".into());
                } else if !self.paused
                    && current
                        .buffering_since
                        .is_some_and(|s| s.elapsed() > Duration::from_secs(60))
                {
                    self.fail("The audio connection stalled.".into());
                } else if current.ready
                    && !self.paused
                    && !current.buffering
                    && current.last_progress.elapsed() > Duration::from_secs(30)
                {
                    self.fail("Audio playback stopped making progress.".into());
                }
            } else if current.ended
                && self.process.is_some()
                && current.started.elapsed() > Duration::from_secs(2)
            {
                self.disconnect();
            }
        }
        self.prepare();
    }
    fn end(&mut self) {
        if let Some(current) = &mut self.current {
            if current.ended {
                return;
            }
            current.ended = true;
            current.awaiting_next = false;
            current.started = Instant::now();
        }
        self.resolver = None;
        self.emit(self.generation(), PlaybackEvent::Ended);
    }
    fn busy(&self) -> bool {
        self.process.is_some() || self.current.as_ref().is_some_and(|c| c.retry_at.is_some())
    }
}
impl<W: Fn()> Drop for Engine<W> {
    fn drop(&mut self) {
        self.resolver = None;
        self.disconnect();
    }
}
fn premature_eof(ready: bool, seekable: bool, position: f64, duration: Option<f64>) -> bool {
    // Stream duration is an estimate and may include a video-only tail. mpv's
    // last observed position can also precede EOF. Retry substantial truncation,
    // but let normal endings hand off to the already prepared playlist entry.
    ready
        && seekable
        && duration.is_some_and(|duration| {
            let tolerance = (duration * 0.02).clamp(5., 10.);
            position + tolerance < duration
        })
}
fn run_engine<W: Fn()>(commands: Receiver<PlayerCommand>, mut engine: Engine<W>) {
    loop {
        let command = if engine.busy() {
            commands.recv_timeout(Duration::from_millis(50))
        } else {
            commands
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match command {
            Ok(command) => {
                if !engine.handle(command) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        // Drain commands before observing events: cancellation and queue edits
        // must take priority over stale prefetch completions.
        while let Ok(command) = commands.try_recv() {
            if !engine.handle(command) {
                return;
            }
        }
        engine.tick();
    }
}
fn player_loop(commands: Receiver<PlayerCommand>, events: Sender<PlayerEvent>, wake: impl Fn()) {
    run_engine(commands, Engine::new(events, wake));
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
                    .filter(|v| v.is_finite())
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
    #[test]
    fn truncated_seekable_streams_are_retried_but_live_and_finished_streams_end() {
        assert!(premature_eof(true, true, 10., Some(180.)));
        assert!(!premature_eof(true, true, 179.8, Some(180.)));
        assert!(!premature_eof(true, true, 304.185527, Some(307.)));
        assert!(premature_eof(true, true, 290., Some(307.)));
        assert!(!premature_eof(true, false, 10., Some(180.)));
        assert!(!premature_eof(true, true, 10., None));
    }
    fn pending_handoff() -> (Engine<fn()>, Receiver<PlayerEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut engine: Engine<fn()> = Engine::new(tx, || {});
        engine.handle(PlayerCommand::Play {
            track: Track::default(),
            generation: 12,
        });
        engine.handle(PlayerCommand::PrepareNext {
            track: Some(Track {
                id: "next".into(),
                ..Default::default()
            }),
            generation: 12,
            revision: 42,
        });
        let current = engine.current.as_mut().unwrap();
        current.ready = true;
        current.seekable = true;
        current.position = 304.185527;
        current.duration = Some(307.);
        current.entry = Some(1);
        let next = engine.next.as_mut().unwrap();
        next.entry = Some(2);
        engine.bindings.insert(2, next.binding.clone());
        engine.active_entry = Some(1);
        engine.message(json!({"event":"end-file", "reason":"eof", "playlist_entry_id":1}));
        assert!(engine.current.as_ref().unwrap().awaiting_next);
        engine.message(json!({"event":"start-file", "playlist_entry_id":2}));
        engine.message(json!({"event":"file-loaded"}));
        assert_eq!(engine.generation(), 13);
        (engine, rx)
    }
    #[test]
    fn estimated_duration_tail_hands_off_at_the_audible_boundary() {
        let (mut engine, rx) = pending_handoff();
        engine.message(json!({"event":"property-change", "name":"duration", "data":383.}));
        engine.message(json!({"event":"property-change", "name":"audio-pts", "data":-3.}));
        engine.message(json!({"event":"property-change", "name":"audio-pts", "data":-1.}));
        let waiting: Vec<_> = rx.try_iter().collect();
        assert!(waiting.iter().all(|e| e.generation == 12
            && matches!(
                e.event,
                PlaybackEvent::Position(_) | PlaybackEvent::Paused(_)
            )));
        assert!((engine.handoff.as_ref().unwrap().previous.position - 306.185527).abs() < 0.001);
        engine.message(json!({"event":"property-change", "name":"audio-pts", "data":null}));
        assert!(engine.handoff.is_some());
        engine.message(json!({"event":"property-change", "name":"audio-pts", "data":0.01}));
        assert!(engine.handoff.is_none());
        let events: Vec<_> = rx.try_iter().collect();
        assert!(matches!(
            events[0].event,
            PlaybackEvent::Transition { revision: 42, .. }
        ));
        assert_eq!(events[0].generation, 12);
        assert!(events[1..].iter().all(|e| e.generation == 13));
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, PlaybackEvent::Duration(383.)))
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, PlaybackEvent::Transition { revision: 42, .. }))
        );
        assert!(events.iter().all(|e| !matches!(
            e.event,
            PlaybackEvent::Recovering | PlaybackEvent::Ended | PlaybackEvent::Error(_)
        )));
    }
    #[test]
    fn seeking_or_recovering_during_the_tail_keeps_the_audible_track() {
        for seek in [false, true] {
            let (mut engine, rx) = pending_handoff();
            if seek {
                engine.handle(PlayerCommand::Seek(37.));
                assert_eq!(engine.current.as_ref().unwrap().position, 37.);
            } else {
                engine.fail("disconnected".into());
                assert!(rx.try_iter().any(|e| e.generation == 12 && matches!(e.event, PlaybackEvent::Recovering)));
            }
            assert_eq!(engine.generation(), 12);
            assert_eq!(
                engine.current.as_ref().unwrap().binding.track,
                Track::default()
            );
            assert_eq!(
                engine.next.as_ref().unwrap().binding.transition,
                Some((12, 42))
            );
            assert!(engine.handoff.is_none());
            assert!(
                rx.try_iter()
                    .all(|e| !matches!(e.event, PlaybackEvent::Transition { .. }))
            );
        }
    }
    #[test]
    fn replacing_or_stopping_discards_an_inaudible_handoff() {
        for stop in [false, true] {
            let (mut engine, rx) = pending_handoff();
            engine.handle(if stop {
                PlayerCommand::Stop
            } else {
                PlayerCommand::Play {
                    track: Track::default(),
                    generation: 20,
                }
            });
            engine.audio_clock(1.);
            assert!(engine.handoff.is_none());
            assert!(
                rx.try_iter()
                    .all(|e| !matches!(e.event, PlaybackEvent::Transition { .. }))
            );
        }
    }
    #[test]
    fn stop_cancels_pending_recovery_and_retry_budget_is_bounded() {
        let (tx, rx) = mpsc::channel();
        let mut engine = Engine::new(tx, || {});
        engine.handle(PlayerCommand::Play {
            track: Track::default(),
            generation: 12,
        });
        for _ in 0..3 {
            engine.fail("disconnected".into());
            if let Some(current) = &mut engine.current {
                current.retry_at = None;
            }
        }
        let events: Vec<_> = rx.try_iter().collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, PlaybackEvent::Recovering))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, PlaybackEvent::Error(_)))
                .count(),
            1
        );
        assert!(engine.current.as_ref().unwrap().ended);
        assert!(!engine.busy());
        engine.handle(PlayerCommand::Play {
            track: Track::default(),
            generation: 13,
        });
        engine.fail("disconnected".into());
        engine.handle(PlayerCommand::Stop);
        engine.tick();
        assert!(engine.current.is_none());
        assert!(engine.process.is_none());
    }
    #[cfg(unix)]
    #[test]
    fn ipc_handles_fragmentation_backpressure_and_disconnection() {
        use std::os::unix::net::UnixStream;
        let (stream, mut peer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut c = Connection {
            stream,
            pending: Vec::new(),
            outgoing: Vec::new(),
            written: 0,
        };
        peer.write_all(b"{\"event\":\"file-").unwrap();
        assert!(c.messages().unwrap().is_empty());
        peer.write_all(b"loaded\"}\n{\"event\":\"seek\"}\n")
            .unwrap();
        assert_eq!(c.messages().unwrap().len(), 2);
        let payload = "x".repeat(4096);
        for index in 0..50 {
            c.send(json!({"index":index,"payload":payload})).unwrap();
        }
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        for _ in 0..100 {
            loop {
                match peer.read(&mut buffer) {
                    Ok(n) if n > 0 => bytes.extend_from_slice(&buffer[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    result => panic!("unexpected read: {result:?}"),
                }
            }
            c.flush().unwrap();
            if bytes.iter().filter(|b| **b == b'\n').count() == 50 {
                break;
            }
        }
        let messages: Vec<Value> = bytes
            .split(|b| *b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(messages.len(), 50);
        for (index, message) in messages.iter().enumerate() {
            assert_eq!(message["index"], index);
            assert_eq!(message["payload"], payload);
        }
        drop(peer);
        assert_eq!(
            c.messages().unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
}

#[cfg(all(test, unix))]
mod integration_tests {
    use super::*;
    struct AudioFile(std::path::PathBuf);
    impl AudioFile {
        fn wav(seconds: u32, sample: i16) -> Self {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "riff-audio-{}-{}.wav",
                std::process::id(),
                SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let size = 48_000 * seconds * 4;
            let mut bytes = Vec::new();
            bytes.extend(b"RIFF");
            bytes.extend((size + 36).to_le_bytes());
            bytes.extend(b"WAVEfmt ");
            bytes.extend(16u32.to_le_bytes());
            bytes.extend(1u16.to_le_bytes());
            bytes.extend(2u16.to_le_bytes());
            bytes.extend(48_000u32.to_le_bytes());
            bytes.extend(192_000u32.to_le_bytes());
            bytes.extend(4u16.to_le_bytes());
            bytes.extend(16u16.to_le_bytes());
            bytes.extend(b"data");
            bytes.extend(size.to_le_bytes());
            for _ in 0..size / 2 {
                bytes.extend(sample.to_le_bytes());
            }
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }
        fn track(&self) -> Track {
            Track {
                id: self.0.to_string_lossy().into_owned(),
                ..Default::default()
            }
        }
    }
    impl Drop for AudioFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    fn local_engine() -> (Engine<impl Fn()>, Receiver<PlayerEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut engine = Engine::new(tx, || {});
        engine.source = |track| track.id.clone();
        engine.command = || {
            let mut command = audio_command();
            command.arg("--ao=null");
            command
        };
        (engine, rx)
    }
    fn drive<W: Fn()>(engine: &mut Engine<W>, done: impl Fn(&Engine<W>) -> bool) {
        let started = Instant::now();
        loop {
            engine.tick();
            if done(engine) {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "playback operation timed out: current={:?}, next={:?}, pending={}",
                engine.current.as_ref().map(|c| (
                    c.binding.generation,
                    c.ready,
                    c.ended,
                    c.retries,
                    c.entry,
                    c.position,
                    c.duration
                )),
                engine.next.as_ref().map(|n| (n.entry, n.attempted)),
                engine.requests.len()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    #[ignore = "requires mpv, yt-dlp and live YouTube access"]
    fn mpv_live_youtube_prefetch_and_native_handoff() {
        live_youtube_handoff("dQw4w9WgXcQ", "hTWKbfoikeg", 1.);
    }
    #[test]
    #[ignore = "requires mpv, yt-dlp and live YouTube access"]
    fn mpv_live_album_tail_handoff() {
        live_youtube_handoff("Eq8ZiZ9xiLw", "-0kcet4aPpQ", 6.);
    }
    fn live_youtube_handoff(first: &str, second: &str, tail: f64) {
        let (tx, events) = mpsc::channel();
        let mut engine = Engine::new(tx, || {});
        engine.command = || {
            let mut command = audio_command();
            command.arg("--ao=null");
            command
        };
        let a = Track {
            id: first.into(),
            ..Default::default()
        };
        let b = Track {
            id: second.into(),
            ..Default::default()
        };
        engine.handle(PlayerCommand::Play {
            track: a,
            generation: 100,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        engine.handle(PlayerCommand::PrepareNext {
            track: Some(b.clone()),
            generation: 100,
            revision: 42,
        });
        let started = Instant::now();
        loop {
            engine.tick();
            if engine
                .current
                .as_ref()
                .is_some_and(|c| c.ready && c.seekable && c.duration.is_some())
                && engine.next.as_ref().is_some_and(|n| n.entry.is_some())
            {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(40),
                "YouTube playback/preparation timed out"
            );
            assert!(
                engine.current.as_ref().is_none_or(|c| !c.ended),
                "YouTube playback failed"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let pid = engine.process.as_ref().unwrap().id();
        let duration = engine.current.as_ref().unwrap().duration.unwrap();
        engine.handle(PlayerCommand::Seek(duration - tail));
        // Seeking ahead lets mpv finish reading this stream and open the next
        // one while its buffered tail is still playing.
        for _ in 0..40 {
            engine.tick();
            thread::sleep(Duration::from_millis(25));
        }
        engine.handle(PlayerCommand::SetPaused(false));
        drive(&mut engine, |e| {
            e.generation() == 101
                && e.handoff.is_none()
                && e.current.as_ref().is_some_and(|c| c.ready)
        });
        assert_eq!(engine.process.as_ref().unwrap().id(), pid);
        assert_eq!(engine.current.as_ref().unwrap().binding.track, b);
        let events: Vec<_> = events.try_iter().collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, PlaybackEvent::Transition { revision: 42, .. }))
                .count(),
            1
        );
        assert!(events.iter().all(|e| !matches!(
            e.event,
            PlaybackEvent::Ended
                | PlaybackEvent::Error(_)
                | PlaybackEvent::Recovering
                | PlaybackEvent::ControlError(_)
        )));
    }
    #[test]
    #[ignore = "requires mpv; verifies UI handoff timing with a large output buffer"]
    fn mpv_handoff_waits_for_output_clock_and_preserves_paused_tail() {
        let a = AudioFile::wav(6, 1000);
        let b = AudioFile::wav(10, -2000);
        let (mut engine, events) = local_engine();
        engine.extra_args.push("--audio-buffer=3".into());
        engine.handle(PlayerCommand::Play {
            track: a.track(),
            generation: 10,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        engine.handle(PlayerCommand::PrepareNext {
            track: Some(b.track()),
            generation: 10,
            revision: 7,
        });
        drive(&mut engine, |e| {
            e.current.as_ref().is_some_and(|c| c.ready)
                && e.next.as_ref().is_some_and(|n| n.entry.is_some())
        });
        events.try_iter().for_each(drop);
        let started = Instant::now();
        engine.handle(PlayerCommand::SetPaused(false));
        drive(&mut engine, |e| {
            e.handoff
                .as_ref()
                .is_some_and(|h| h.audio_pts.is_some_and(|p| p < -1.))
        });
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            events
                .try_iter()
                .all(|e| !matches!(e.event, PlaybackEvent::Transition { .. }))
        );
        engine.handle(PlayerCommand::SetPaused(true));
        let paused_at = Instant::now();
        while paused_at.elapsed() < Duration::from_millis(400) {
            engine.tick();
            thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.handoff.is_some());
        assert!(
            events
                .try_iter()
                .all(|e| !matches!(e.event, PlaybackEvent::Transition { .. }))
        );
        let paused_for = paused_at.elapsed();
        engine.handle(PlayerCommand::SetPaused(false));
        drive(&mut engine, |e| e.handoff.is_none());
        let audible_time = started.elapsed() - paused_for;
        assert!(
            audible_time >= Duration::from_millis(5700),
            "early UI handoff at {audible_time:?}"
        );
        assert!(
            audible_time < Duration::from_millis(6700),
            "late UI handoff at {audible_time:?}"
        );
        let transitions: Vec<_> = events
            .try_iter()
            .filter(|e| matches!(e.event, PlaybackEvent::Transition { revision: 7, .. }))
            .collect();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].generation, 10);
    }
    #[test]
    #[ignore = "requires mpv; verifies seek/recovery while the previous tail is audible"]
    fn mpv_tail_seek_and_crash_keep_the_audible_track() {
        let a = AudioFile::wav(6, 1000);
        let b = AudioFile::wav(10, -2000);
        for crash in [false, true] {
            let (mut engine, events) = local_engine();
            engine.extra_args.push("--audio-buffer=3".into());
            engine.handle(PlayerCommand::Play {
                track: a.track(),
                generation: 10,
            });
            engine.handle(PlayerCommand::SetPaused(true));
            engine.handle(PlayerCommand::PrepareNext {
                track: Some(b.track()),
                generation: 10,
                revision: 7,
            });
            drive(&mut engine, |e| {
                e.next.as_ref().is_some_and(|n| n.entry.is_some())
            });
            engine.handle(PlayerCommand::SetPaused(false));
            drive(&mut engine, |e| {
                e.handoff
                    .as_ref()
                    .is_some_and(|h| h.audio_pts.is_some_and(|p| p < -1.))
            });
            engine.handle(PlayerCommand::SetPaused(true));
            let position = if crash {
                let position = engine.handoff.as_ref().unwrap().previous.position;
                engine.process.as_mut().unwrap().kill().unwrap();
                engine.process.as_mut().unwrap().wait().unwrap();
                position
            } else {
                engine.handle(PlayerCommand::Seek(1.));
                1.
            };
            drive(&mut engine, |e| {
                e.generation() == 10
                    && e.handoff.is_none()
                    && e.current.as_ref().is_some_and(|c| c.ready && c.seekable)
                    && e.next.as_ref().is_some_and(|n| n.entry.is_some())
            });
            assert_eq!(engine.current.as_ref().unwrap().binding.track, a.track());
            assert!((engine.current.as_ref().unwrap().position - position).abs() < 0.2);
            assert!(engine.paused);
            assert!(events.try_iter().all(|e| !matches!(
                e.event,
                PlaybackEvent::Transition { .. } | PlaybackEvent::Error(_)
            )));
        }
    }
    #[test]
    #[ignore = "requires mpv; verifies the final track's buffered tail is not cut off"]
    fn mpv_final_track_ends_after_output_drains() {
        let a = AudioFile::wav(6, 1000);
        let (mut engine, events) = local_engine();
        engine.extra_args.push("--audio-buffer=3".into());
        engine.handle(PlayerCommand::Play {
            track: a.track(),
            generation: 10,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        drive(&mut engine, |e| e.current.as_ref().is_some_and(|c| c.ready));
        events.try_iter().for_each(drop);
        let started = Instant::now();
        engine.handle(PlayerCommand::SetPaused(false));
        drive(&mut engine, |e| e.current.as_ref().is_some_and(|c| c.ended));
        assert!(
            started.elapsed() >= Duration::from_millis(5700),
            "early EOF at {:?}",
            started.elapsed()
        );
        assert!(
            events
                .try_iter()
                .any(|e| matches!(e.event, PlaybackEvent::Ended))
        );
    }
    #[test]
    #[ignore = "requires mpv; verifies exact contiguous PCM output from the engine"]
    fn mpv_engine_gapless_pcm_has_no_missing_or_inserted_samples() {
        let a = AudioFile::wav(1, 1000);
        let b = AudioFile::wav(1, -2000);
        let output = AudioFile(a.0.with_extension("output.wav"));
        let (mut engine, events) = local_engine();
        engine.command = audio_command;
        engine.extra_args = vec![
            "--ao=pcm".into(),
            "--audio-format=s16".into(),
            // The file-output AO is untimed; pace decoding without changing
            // samples so the worker can prepare a genuinely live handoff.
            "--af=lavfi=[arealtime]".into(),
            format!("--ao-pcm-file={}", output.0.display()),
        ];
        engine.handle(PlayerCommand::SetVolume(100));
        engine.handle(PlayerCommand::Play {
            track: a.track(),
            generation: 10,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        engine.handle(PlayerCommand::PrepareNext {
            track: Some(b.track()),
            generation: 10,
            revision: 7,
        });
        drive(&mut engine, |e| {
            e.next.as_ref().is_some_and(|n| n.entry.is_some())
        });
        let pid = engine.process.as_ref().unwrap().id();
        engine.handle(PlayerCommand::SetPaused(false));
        drive(&mut engine, |e| {
            e.generation() == 11 && e.current.as_ref().is_some_and(|c| c.ended)
        });
        assert_eq!(engine.process.as_ref().unwrap().id(), pid);
        let events: Vec<_> = events.try_iter().collect();
        assert_eq!(
            events
                .iter()
                .filter(
                    |e| matches!(e.event, PlaybackEvent::Transition { revision: 7, .. })
                        && e.generation == 10
                )
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, PlaybackEvent::Ended))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e.event, PlaybackEvent::Error(_) | PlaybackEvent::Recovering))
        );
        engine
            .connection
            .as_mut()
            .unwrap()
            .send(json!({"command":["quit"]}))
            .unwrap();
        let started = Instant::now();
        while engine
            .process
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none()
        {
            assert!(started.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(10));
        }
        engine.handle(PlayerCommand::Stop);
        let bytes = std::fs::read(&output.0).unwrap();
        let mut offset = 12;
        let pcm = loop {
            let size =
                u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
            if &bytes[offset..offset + 4] == b"data" {
                break &bytes[offset + 8..offset + 8 + size];
            }
            offset += 8 + size + size % 2;
        };
        assert_eq!(pcm.len(), 48_000 * 2 * 2 * 2);
        let samples: Vec<_> = pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert!(samples[..96_000].iter().all(|sample| *sample == 1000));
        assert!(samples[96_000..].iter().all(|sample| *sample == -2000));
    }
    #[test]
    #[ignore = "requires mpv; verifies process recovery, resume, pause and volume"]
    fn mpv_engine_recovers_crash_without_advancing_queue() {
        let a = AudioFile::wav(8, 500);
        let b = AudioFile::wav(8, 1000);
        let (mut engine, events) = local_engine();
        engine.handle(PlayerCommand::Play {
            track: a.track(),
            generation: 5,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        engine.handle(PlayerCommand::SetVolume(37));
        engine.handle(PlayerCommand::PrepareNext {
            track: Some(b.track()),
            generation: 5,
            revision: 2,
        });
        drive(&mut engine, |e| {
            e.current.as_ref().is_some_and(|c| c.ready && c.seekable)
        });
        engine.handle(PlayerCommand::Seek(3.2));
        let pid = engine.process.as_ref().unwrap().id();
        engine.process.as_mut().unwrap().kill().unwrap();
        drive(&mut engine, |e| {
            e.current
                .as_ref()
                .is_some_and(|c| c.retries == 1 && c.ready)
        });
        assert_ne!(engine.process.as_ref().unwrap().id(), pid);
        assert_eq!(engine.generation(), 5);
        assert!(engine.paused);
        assert_eq!(engine.volume, 37);
        drive(&mut engine, |e| {
            e.current.as_ref().is_some_and(|c| c.position >= 3.)
        });
        assert!((engine.current.as_ref().unwrap().position - 3.2).abs() < 0.3);
        assert_eq!(engine.next.as_ref().unwrap().binding.track, b.track());
        let events: Vec<_> = events.try_iter().collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, PlaybackEvent::Recovering))
                .count(),
            1
        );
        assert!(events.iter().all(|e| e.generation == 5
            && !matches!(
                e.event,
                PlaybackEvent::Ended | PlaybackEvent::Transition { .. } | PlaybackEvent::Error(_)
            )));
        assert!(
            events
                .iter()
                .any(|e| matches!(e.event, PlaybackEvent::Paused(true)))
        );
    }
    #[test]
    #[ignore = "requires mpv; verifies an unresponsive paused player is recovered"]
    fn mpv_engine_recovers_hung_ipc_even_while_paused() {
        let a = AudioFile::wav(8, 500);
        let (mut engine, events) = local_engine();
        engine.handle(PlayerCommand::Play {
            track: a.track(),
            generation: 8,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        drive(&mut engine, |e| e.current.as_ref().is_some_and(|c| c.ready));
        let pid = engine.process.as_ref().unwrap().id();
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
        // Move the deadline forward rather than make the suite sleep 10 seconds.
        engine.heartbeat_sent = Some(Instant::now() - Duration::from_secs(11));
        drive(&mut engine, |e| {
            e.current
                .as_ref()
                .is_some_and(|c| c.retries == 1 && c.ready)
        });
        assert_ne!(engine.process.as_ref().unwrap().id(), pid);
        assert!(engine.paused);
        assert!(
            events
                .try_iter()
                .any(|e| e.generation == 8 && matches!(e.event, PlaybackEvent::Recovering))
        );
    }
    #[test]
    #[ignore = "requires mpv; verifies cancellation, replacement and terminal errors"]
    fn mpv_engine_queue_changes_and_unavailable_audio_are_bounded() {
        let a = AudioFile::wav(3, 500);
        let b = AudioFile::wav(3, 1000);
        let (mut engine, events) = local_engine();
        engine.handle(PlayerCommand::Play {
            track: a.track(),
            generation: 1,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        engine.handle(PlayerCommand::PrepareNext {
            track: Some(b.track()),
            generation: 1,
            revision: 1,
        });
        drive(&mut engine, |e| {
            e.next.as_ref().is_some_and(|n| n.entry.is_some())
        });
        let pid = engine.process.as_ref().unwrap().id();
        engine.handle(PlayerCommand::PrepareNext {
            track: None,
            generation: 1,
            revision: 2,
        });
        engine.handle(PlayerCommand::Seek(2.8));
        engine.handle(PlayerCommand::SetPaused(false));
        drive(&mut engine, |e| e.current.as_ref().is_some_and(|c| c.ended));
        assert!(
            events
                .try_iter()
                .all(|e| !matches!(e.event, PlaybackEvent::Transition { .. }))
        );
        engine.handle(PlayerCommand::Play {
            track: b.track(),
            generation: 2,
        });
        engine.handle(PlayerCommand::SetPaused(true));
        drive(&mut engine, |e| e.current.as_ref().is_some_and(|c| c.ready));
        assert_eq!(engine.process.as_ref().unwrap().id(), pid);
        engine.handle(PlayerCommand::Play {
            track: Track {
                id: a
                    .0
                    .with_extension("missing.wav")
                    .to_string_lossy()
                    .into_owned(),
                ..Default::default()
            },
            generation: 3,
        });
        drive(&mut engine, |e| e.current.as_ref().is_some_and(|c| c.ended));
        assert_eq!(engine.current.as_ref().unwrap().retries, 2);
        assert!(engine.process.is_none());
        let events: Vec<_> = events.try_iter().filter(|e| e.generation == 3).collect();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.event, PlaybackEvent::Error(_)))
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e.event, PlaybackEvent::Ended))
        );
    }
    #[test]
    #[ignore = "requires mpv on PATH; exercises real JSON IPC with local audio"]
    fn mpv_pause_volume_and_seek() {
        let path = ipc_path(999_999);
        let wav = std::env::temp_dir().join(format!("riff-ipc-test-{}.wav", std::process::id()));
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
        std::fs::write(&wav, bytes).unwrap();
        let child = Child::spawn(
            audio_command()
                .args([
                    "--ao=null",
                    "--pause=yes",
                    "--idle=yes",
                    &format!("--input-ipc-server={path}"),
                ])
                .arg(&wav)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
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

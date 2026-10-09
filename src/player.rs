//! mpv lives on a worker; its JSON IPC observations are the source of playback truth.
use crate::model::Track;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

pub enum PlayerCommand {
    Play { track: Track, generation: u64 },
    SetPaused(bool),
    SetVolume(u8),
    Seek(f64),
    Stop,
    Shutdown,
}
#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    Ready,
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
                    return Err(std::io::Error::last_os_error());
                }
                if available == 0 {
                    break;
                }
            }
            match self.stream.read(&mut buf) {
                Ok(0) => break,
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

fn player_loop(commands: Receiver<PlayerCommand>, events: Sender<PlayerEvent>, wake: impl Fn()) {
    let mut process: Option<Child> = None;
    let mut ipc = None;
    let mut connection: Option<Connection> = None;
    let mut volume = 70;
    let mut paused = false;
    let mut generation = 0;
    let mut ready = false;
    let mut terminal_event = false;
    let mut started = Instant::now();
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
        // Nothing needs polling before playback or after Stop. A command wakes
        // the worker immediately; only a running mpv needs regular IPC checks.
        let command = if process.is_some() {
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
            }) => {
                terminate(&mut process, &mut ipc);
                connection = None;
                generation = next;
                ready = false;
                terminal_event = false;
                paused = false;
                let path = ipc_path(generation);
                match audio_command()
                    .args([
                        &format!("--volume={volume}"),
                        &format!("--input-ipc-server={path}"),
                        "--",
                        &track.youtube_url(),
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        process = Some(child);
                        ipc = Some(path);
                        started = Instant::now();
                    }
                    Err(e) => emit(
                        generation,
                        PlaybackEvent::Error(format!(
                            "Couldn't start mpv: {e}. Install mpv and make sure it is on PATH."
                        )),
                    ),
                }
            }
            Ok(PlayerCommand::SetPaused(value)) => {
                paused = value;
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
                if position.is_finite()
                    && let Some(c) = &mut connection
                    && let Err(e) = c.send(
                        json!({"command":["seek",position.max(0.0),"absolute+exact"],"request_id":100}),
                    )
                {
                    emit(generation, PlaybackEvent::ControlError(e.to_string()));
                }
            }
            Ok(PlayerCommand::Stop) => {
                terminate(&mut process, &mut ipc);
                connection = None;
                emit(generation, PlaybackEvent::Stopped);
            }
            Ok(PlayerCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                terminate(&mut process, &mut ipc);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if connection.is_none()
            && let Some(path) = &ipc
            && let Ok(mut c) = Connection::open(path)
        {
            let properties = [
                "time-pos",
                "duration",
                "pause",
                "paused-for-cache",
                "seekable",
                "volume",
            ];
            for (i, property) in properties.iter().enumerate() {
                let _ = c.send(json!({"command":["observe_property",i+1,property]}));
            }
            let _ = c.send(json!({"command":["set_property","volume",volume]}));
            let _ = c.send(json!({"command":["set_property","pause",paused]}));
            connection = Some(c);
        }
        if let Some(c) = &mut connection {
            match c.messages() {
                Ok(messages) => {
                    for message in messages {
                        if let Some(event) = parse_message(&message) {
                            if matches!(event, PlaybackEvent::Ready) {
                                ready = true;
                            }
                            if matches!(event, PlaybackEvent::Ended | PlaybackEvent::Error(_)) {
                                terminal_event = true;
                            }
                            emit(generation, event);
                        }
                    }
                }
                Err(e) => {
                    emit(
                        generation,
                        PlaybackEvent::Error(format!(
                            "Lost contact with the player: {e}. Try playing the track again."
                        )),
                    );
                    terminal_event = true;
                    terminate(&mut process, &mut ipc);
                    connection = None;
                }
            }
        }
        if process.is_some() && !ready && started.elapsed() > Duration::from_secs(45) {
            terminate(&mut process, &mut ipc);
            connection = None;
            emit(generation, PlaybackEvent::Error("This track took too long to load. Check your connection, then retry or choose another track.".into()));
        }
        if let Some(child) = process.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    process = None;
                    cleanup(&mut ipc);
                    connection = None;
                    if !terminal_event {
                        emit(
                            generation,
                            if status.success() && ready {
                                PlaybackEvent::Ended
                            } else {
                                PlaybackEvent::Error("This track couldn't be played. It may be unavailable. Try another track, or update mpv and yt-dlp.".into())
                            },
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    terminate(&mut process, &mut ipc);
                    connection = None;
                    emit(generation, PlaybackEvent::Error(e.to_string()));
                }
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

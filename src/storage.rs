//! Small local collections. A single writer serializes snapshots off the UI thread.
use crate::model::{Track, is_valid_video_id};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub recent: Vec<Track>,
    pub queue: Vec<Track>,
    pub volume: u8,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            recent: Vec::new(),
            queue: Vec::new(),
            volume: 70,
        }
    }
}
fn config_dir() -> Result<PathBuf, String> {
    #[cfg(feature = "ui-qa")]
    if let Some(path) = std::env::var_os("RIFF_QA_CONFIG_DIR") {
        return Ok(path.into());
    }
    ProjectDirs::from("dev", "riff", "Riff")
        .map(|d| d.config_dir().to_owned())
        .ok_or_else(|| "Cannot locate the application configuration folder".into())
}
fn load<T: serde::de::DeserializeOwned + Default>(name: &str) -> Result<T, String> {
    let path = config_dir()?.join(name);
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| format!("Couldn't read {name}: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("Couldn't read {name}: {e}")),
    }
}
pub fn load_library() -> (Vec<Track>, Session, Option<String>) {
    let favorites = load::<Vec<Track>>("favorites.json");
    let session = load::<Session>("session.json");
    let error = favorites
        .as_ref()
        .err()
        .or_else(|| session.as_ref().err())
        .cloned();
    let mut favorites = favorites.unwrap_or_default();
    let mut session = session.unwrap_or_default();
    sanitize(&mut favorites);
    sanitize(&mut session.recent);
    sanitize(&mut session.queue);
    session.recent.truncate(40);
    session.volume = session.volume.min(100);
    (favorites, session, error)
}
fn sanitize(tracks: &mut Vec<Track>) {
    tracks.retain(|t| is_valid_video_id(&t.id) && !t.title.trim().is_empty());
}
struct Snapshot {
    favorites: Vec<Track>,
    session: Session,
}
pub struct LibraryWriter {
    tx: Option<SyncSender<()>>,
    pending: Arc<Mutex<Option<Snapshot>>>,
    pub errors: Receiver<String>,
    worker: Option<thread::JoinHandle<()>>,
}
impl LibraryWriter {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        Self::with_save(ctx, |snapshot| {
            save("favorites.json", &snapshot.favorites)
                .and_then(|()| save("session.json", &snapshot.session))
        })
    }
    fn with_save(
        ctx: eframe::egui::Context,
        mut write: impl FnMut(Snapshot) -> Result<(), String> + Send + 'static,
    ) -> Self {
        // One latest snapshot and one notification, even when disk writes are
        // slow. Quick edits replace queued collections instead of retaining
        // an unbounded copy of the entire library for every slider movement.
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let pending = Arc::new(Mutex::new(None));
        let work = pending.clone();
        let (error_tx, errors) = mpsc::channel();
        let worker = thread::spawn(move || {
            while rx.recv().is_ok() {
                // Coalesce quick edits and slider drags into one durable write.
                while rx.recv_timeout(Duration::from_millis(180)).is_ok() {}
                let snapshot = work.lock().unwrap().take();
                if let Some(snapshot) = snapshot
                    && let Err(e) = write(snapshot)
                {
                    let _ = error_tx.send(e);
                    ctx.request_repaint();
                }
            }
        });
        Self {
            tx: Some(tx),
            pending,
            errors,
            worker: Some(worker),
        }
    }
    pub fn save(&self, favorites: &[Track], session: Session) {
        if let Some(tx) = &self.tx {
            *self.pending.lock().unwrap() = Some(Snapshot {
                favorites: favorites.to_vec(),
                session,
            });
            let _ = tx.try_send(());
        }
    }
}
impl Drop for LibraryWriter {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn save<T: Serialize>(name: &str, data: &T) -> Result<(), String> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(name);
    save_to(&path, data)
}
fn save_to<T: Serialize>(path: &std::path::Path, data: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(data).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    {
        fs::write(path, bytes).map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, bytes).map_err(|e| e.to_string())?;
        fs::rename(temp, path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_writer_keeps_only_latest_snapshot_and_flushes_on_exit() {
        let (written, writes) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let mut first = true;
        let writer = LibraryWriter::with_save(eframe::egui::Context::default(), move |snapshot| {
            written.send(snapshot.session.volume).unwrap();
            if first {
                first = false;
                blocked.recv().unwrap();
            }
            Ok(())
        });
        writer.save(
            &[],
            Session {
                volume: 1,
                ..Default::default()
            },
        );
        let first_write = writes.recv_timeout(Duration::from_secs(3));
        // Simulate edits while an earlier write is blocked on slow storage.
        for volume in 2..=100 {
            writer.save(
                &[],
                Session {
                    volume,
                    ..Default::default()
                },
            );
        }
        let pending_volume = writer
            .pending
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.session.volume);
        release.send(()).unwrap();
        drop(writer);
        assert_eq!(first_write.unwrap(), 1);
        assert_eq!(pending_volume, Some(100));
        assert_eq!(writes.into_iter().collect::<Vec<_>>(), [100]);
    }
    #[test]
    fn persistence_replaces_files_and_keeps_legacy_favorites() {
        let dir = std::env::temp_dir().join(format!(
            "riff-storage-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let track = Track {
            id: "dQw4w9WgXcQ".into(),
            title: "Favorite".into(),
            artist: "Artist".into(),
            duration_seconds: Some(213),
        };
        let path = dir.join("favorites.json");
        save_to(&path, &vec![track.clone()]).unwrap();
        let loaded: Vec<Track> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded, vec![track.clone()]);
        save_to(&path, &Vec::<Track>::new()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "[]");
        let path = dir.join("session.json");
        let session = Session {
            recent: vec![track.clone()],
            queue: vec![track],
            volume: 37,
        };
        save_to(&path, &session).unwrap();
        let loaded: Session = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(loaded.queue, session.queue);
        assert_eq!(loaded.recent, session.recent);
        assert_eq!(loaded.volume, 37);
        let old: Session = serde_json::from_str("{}").unwrap();
        assert_eq!(old.volume, 70);
        assert!(old.queue.is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}

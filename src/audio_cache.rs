use crate::model::is_valid_video_id;
use directories::ProjectDirs;
use std::{
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const CACHE_BYTES: u64 = 512 * 1024 * 1024;
const TRACK_BYTES: u64 = 64 * 1024 * 1024;
const CACHE_ENTRIES: usize = 512;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

pub struct AudioCache {
    dir: Option<PathBuf>,
}

impl AudioCache {
    pub fn new() -> Self {
        Self::at(ProjectDirs::from("dev", "riff", "Riff").map(|d| d.cache_dir().join("audio")))
    }

    pub(crate) fn at(dir: Option<PathBuf>) -> Self {
        let dir = dir.filter(|dir| fs::create_dir_all(dir).is_ok());
        let cache = Self { dir };
        if let Some(dir) = &cache.dir
            && let Ok(entries) = fs::read_dir(dir)
        {
            // A killed app can leave an unpublished download. Never reuse it.
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("download-")
                    && entry.file_type().is_ok_and(|t| t.is_dir())
                    && entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > DOWNLOAD_TIMEOUT)
                {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
        }
        cache.trim(None, CACHE_BYTES, CACHE_ENTRIES);
        cache
    }

    pub fn lookup(&self, id: &str) -> Option<PathBuf> {
        if !is_valid_video_id(id) {
            return None;
        }
        let path = self.dir.as_ref()?.join(format!("{id}.audio"));
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > TRACK_BYTES {
            let _ = fs::remove_file(path);
            return None;
        }
        if let Ok(file) = File::open(&path) {
            let _ = file.set_times(FileTimes::new().set_modified(SystemTime::now()));
        }
        Some(path)
    }

    pub fn invalidate(&self, id: &str) {
        if is_valid_video_id(id)
            && let Some(dir) = &self.dir
        {
            let _ = fs::remove_file(dir.join(format!("{id}.audio")));
        }
    }

    pub(crate) fn publisher(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }

    pub(crate) fn directory(&self) -> Option<PathBuf> {
        self.dir.clone()
    }

    pub(crate) fn publish(&self, staging: &Path, id: &str) -> std::io::Result<()> {
        let root = self
            .dir
            .as_ref()
            .ok_or_else(|| std::io::Error::other("Cache unavailable"))?;
        let mut files = fs::read_dir(staging)?.collect::<Result<Vec<_>, _>>()?;
        // Partial downloads and fragment sidecars are never playable entries.
        if files.len() != 1 || !is_valid_video_id(id) {
            return Err(std::io::Error::other("Incomplete audio download"));
        }
        let entry = files.pop().unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let metadata = fs::symlink_metadata(entry.path())?;
        if !name.starts_with("audio.")
            || name.ends_with(".part")
            || name.ends_with(".ytdl")
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > TRACK_BYTES
        {
            return Err(std::io::Error::other("Invalid audio download"));
        }
        let target = root.join(format!("{id}.audio"));
        // Another app instance may already have published this track.
        if !target.exists() {
            fs::rename(entry.path(), target)?;
        }
        self.trim(Some(id), CACHE_BYTES, CACHE_ENTRIES);
        Ok(())
    }

    fn trim(&self, protected: Option<&str>, budget: u64, limit: usize) {
        let Some(root) = &self.dir else { return };
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        let mut files: Vec<_> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let id = path.file_stem()?.to_str()?;
                if path.extension()? != "audio" || !is_valid_video_id(id) {
                    return None;
                }
                let metadata = fs::symlink_metadata(&path).ok()?;
                if !metadata.is_file() || metadata.len() == 0 || metadata.len() > TRACK_BYTES {
                    let _ = fs::remove_file(path);
                    return None;
                }
                let keep = Some(id) == protected;
                Some((
                    path,
                    metadata.len(),
                    metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    keep,
                ))
            })
            .collect();
        let mut bytes: u64 = files.iter().map(|f| f.1).sum();
        let mut count = files.len();
        files.sort_unstable_by_key(|f| f.2);
        for (path, size, _, keep) in files {
            if bytes <= budget && count <= limit {
                break;
            }
            if !keep && fs::remove_file(path).is_ok() {
                bytes -= size;
                count -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: PathBuf,
        cache: AudioCache,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "riff-audio-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let cache = AudioCache::at(Some(dir.clone()));
            Self { dir, cache }
        }
        fn staging(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let dir = self.dir.join("staging");
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join(name), bytes).unwrap();
            dir
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn only_completed_downloads_are_published_and_survive_restart() {
        let fixture = Fixture::new();
        let id = "dQw4w9WgXcQ";
        let staging = fixture.staging("audio.webm.part", b"partial");
        assert!(fixture.cache.publish(&staging, id).is_err());
        assert!(fixture.cache.lookup(id).is_none());
        fs::rename(staging.join("audio.webm.part"), staging.join("audio.webm")).unwrap();
        fixture.cache.publish(&staging, id).unwrap();
        let restarted = AudioCache::at(Some(fixture.dir.clone()));
        assert_eq!(fs::read(restarted.lookup(id).unwrap()).unwrap(), b"partial");
        restarted.invalidate(id);
        assert!(restarted.lookup(id).is_none());
    }

    #[test]
    fn eviction_bounds_bytes_and_count_and_lookup_refreshes_recency() {
        let fixture = Fixture::new();
        for (i, id) in ["dQw4w9WgXcQ", "hTWKbfoikeg", "4NRXx6U8ABQ"]
            .iter()
            .enumerate()
        {
            let path = fixture.dir.join(format!("{id}.audio"));
            fs::write(&path, [0; 4]).unwrap();
            File::open(path)
                .unwrap()
                .set_times(
                    FileTimes::new()
                        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(i as u64)),
                )
                .unwrap();
        }
        fixture.cache.lookup("dQw4w9WgXcQ").unwrap();
        fixture.cache.trim(Some("4NRXx6U8ABQ"), 8, 2);
        assert!(fixture.cache.lookup("hTWKbfoikeg").is_none());
        assert!(fixture.cache.lookup("dQw4w9WgXcQ").is_some());
        fixture.cache.trim(None, 8, 1);
        assert!(fixture.cache.lookup("4NRXx6U8ABQ").is_none());
        assert!(fixture.cache.lookup("dQw4w9WgXcQ").is_some());
    }

    #[test]
    fn rejects_empty_oversized_and_unsafe_entries() {
        let fixture = Fixture::new();
        let id = "dQw4w9WgXcQ";
        let path = fixture.dir.join(format!("{id}.audio"));
        File::create(&path).unwrap();
        assert!(fixture.cache.lookup(id).is_none());
        File::create(&path)
            .unwrap()
            .set_len(TRACK_BYTES + 1)
            .unwrap();
        assert!(fixture.cache.lookup(id).is_none());
        assert!(fixture.cache.lookup("../escape").is_none());
        #[cfg(unix)]
        {
            let outside = fixture.dir.join("outside");
            fs::write(&outside, b"untouched").unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();
            assert!(fixture.cache.lookup(id).is_none());
            assert_eq!(fs::read(outside).unwrap(), b"untouched");
        }
    }
}

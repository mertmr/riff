//! A bounded artwork pipeline. Only GPU textures remain in the UI cache;
//! compressed bytes and decoded pixels are released as soon as they're uploaded.
use super::{icons::Icon, widgets};
use crate::model::{Track, is_valid_video_id};
use eframe::egui::{self, ColorImage, Rect, TextureHandle, TextureOptions, Ui, pos2};
use std::{
    collections::HashMap,
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const TEXTURE_BUDGET: usize = 8 * 1024 * 1024;
const MAX_DOWNLOAD: u64 = 1024 * 1024;
const DISK_BUDGET: u64 = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 128;

enum State {
    Pending,
    Ready(TextureHandle),
    Failed(Instant),
}
struct Entry {
    state: State,
    used: u64,
    ticket: u64,
    bytes: usize,
}
struct Job {
    id: String,
    ticket: u64,
    url: String,
    size: u32,
    ctx: egui::Context,
}
struct Loaded {
    id: String,
    ticket: u64,
    image: Option<ColorImage>,
}
pub struct Artwork {
    hero: Option<TextureHandle>,
    entries: HashMap<String, Entry>,
    clock: u64,
    next_ticket: u64,
    retained: usize,
    jobs: SyncSender<Job>,
    results: Receiver<Loaded>,
    shutdown: Arc<AtomicBool>,
}
impl Default for Artwork {
    fn default() -> Self {
        // Fixed concurrency and bounded mailboxes prevent fast scrolling from
        // creating one HTTP/decoder thread and a full-size image per song.
        let (jobs, work) = mpsc::sync_channel::<Job>(8);
        let (done, results) = mpsc::sync_channel::<Loaded>(4);
        let work = Arc::new(Mutex::new(work));
        let shutdown = Arc::new(AtomicBool::new(false));
        let dir = directories::ProjectDirs::from("dev", "riff", "Riff")
            .map(|d| d.cache_dir().join("artwork"));
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .timeout_connect(Some(Duration::from_secs(5)))
            .max_idle_connections(2)
            .max_idle_connections_per_host(2)
            .build()
            .into();
        for n in 0..2 {
            let work = work.clone();
            let done = done.clone();
            let shutdown = shutdown.clone();
            let dir = dir.clone();
            let agent = agent.clone();
            thread::Builder::new()
                .name(format!("artwork-{n}"))
                .spawn(move || {
                    loop {
                        // Release the receiver lock before doing network work.
                        let job = work.lock().unwrap().recv();
                        let Ok(job) = job else { break };
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        let image =
                            fetch_image(&agent, dir.as_deref(), &job.id, &job.url, job.size).ok();
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        if done
                            .send(Loaded {
                                id: job.id,
                                ticket: job.ticket,
                                image,
                            })
                            .is_err()
                        {
                            break;
                        }
                        job.ctx.request_repaint();
                    }
                })
                .expect("couldn't start artwork worker");
        }
        Self {
            hero: None,
            entries: HashMap::new(),
            clock: 0,
            next_ticket: 0,
            retained: 0,
            jobs,
            results,
            shutdown,
        }
    }
}
impl Artwork {
    #[cfg(feature = "ui-qa")]
    pub fn is_loading(&self) -> bool {
        self.entries
            .values()
            .any(|entry| matches!(entry.state, State::Pending))
    }
    #[cfg(feature = "ui-qa")]
    pub fn loaded_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| matches!(entry.state, State::Ready(_)))
            .count()
    }
    fn receive(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        while let Ok(loaded) = self.results.try_recv() {
            let Some(entry) = self.entries.get_mut(&loaded.id) else {
                continue;
            };
            if entry.ticket != loaded.ticket {
                continue;
            }
            changed = true;
            entry.state = if let Some(image) = loaded.image {
                entry.bytes = image.pixels.len() * 4;
                self.retained += entry.bytes;
                State::Ready(ctx.load_texture(
                    format!("cover:{}", loaded.id),
                    image,
                    TextureOptions::LINEAR,
                ))
            } else {
                State::Failed(Instant::now() + Duration::from_secs(60))
            };
        }
        if changed {
            // Results can arrive during the final trim, after covers were
            // painted. Ensure the uploaded image appears even in an idle UI.
            ctx.request_repaint();
        }
    }
    pub fn paint_hero(&mut self, ui: &Ui, rect: Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
        let texture = self.hero.get_or_insert_with(|| {
            let image =
                image::load_from_memory(include_bytes!("../../assets/images/music-hero.jpg"))
                    .expect("bundled music hero must be a valid JPEG")
                    .into_rgba8();
            ui.ctx().load_texture(
                "music-hero",
                ColorImage::from_rgba_unmultiplied(
                    [image.width() as usize, image.height() as usize],
                    image.as_raw(),
                ),
                TextureOptions::LINEAR,
            )
        });
        let source = texture.size_vec2();
        let visible = (source.x / source.y) / rect.aspect_ratio();
        let uv = if visible <= 1. {
            Rect::from_min_max(pos2(0., 0.), pos2(1., visible))
        } else {
            Rect::from_min_max(pos2(0., 0.), pos2(1. / visible, 1.))
        };
        egui::Image::from_texture(&*texture)
            .uv(uv)
            .corner_radius(12)
            .paint_at(ui, rect);
    }
    pub fn paint_profile(&mut self, ui: &Ui, url: &str, rect: Rect) -> bool {
        let id = format!("profile:{url}");
        self.paint_url(ui, &id, url, rect, 18)
    }
    pub fn paint(&mut self, ui: &Ui, track: &Track, rect: Rect, radius: u8) {
        if !is_valid_video_id(&track.id) {
            widgets::placeholder(ui, rect, radius, Icon::Music);
            return;
        }
        self.paint_url(
            ui,
            &track.id,
            &Track::artwork_url_for(&track.id),
            rect,
            radius,
        );
    }
    pub fn paint_url(&mut self, ui: &Ui, id: &str, url: &str, rect: Rect, radius: u8) -> bool {
        self.receive(ui.ctx());
        if !ui.is_rect_visible(rect) {
            return false;
        }
        let size = artwork_size(rect.width().max(rect.height()), ui.ctx().pixels_per_point());
        let id = format!("{id}:{size}");
        self.clock = self.clock.wrapping_add(1);
        let retry = self.entries.get(&id).is_none_or(
            |entry| matches!(entry.state, State::Failed(until) if Instant::now() >= until),
        );
        if retry && valid_artwork_url(url) {
            self.next_ticket = self.next_ticket.wrapping_add(1);
            if self
                .jobs
                .try_send(Job {
                    id: id.clone(),
                    ticket: self.next_ticket,
                    url: sized_artwork_url(url, size),
                    size,
                    ctx: ui.ctx().clone(),
                })
                .is_ok()
            {
                self.entries.insert(
                    id.clone(),
                    Entry {
                        state: State::Pending,
                        used: self.clock,
                        ticket: self.next_ticket,
                        bytes: 0,
                    },
                );
            }
        }
        let Some(entry) = self.entries.get_mut(&id) else {
            widgets::placeholder(ui, rect, radius, Icon::Music);
            return false;
        };
        entry.used = self.clock;
        if let State::Ready(texture) = &entry.state {
            let [w, h] = texture.size();
            let ratio = w as f32 / h as f32;
            let uv = if ratio > 1. {
                let width = 1. / ratio;
                Rect::from_min_max(pos2((1. - width) / 2., 0.), pos2((1. + width) / 2., 1.))
            } else {
                Rect::from_min_max(pos2(0., (1. - ratio) / 2.), pos2(1., (1. + ratio) / 2.))
            };
            egui::Image::from_texture(texture)
                .uv(uv)
                .corner_radius(radius)
                .paint_at(ui, rect);
            true
        } else {
            widgets::placeholder(ui, rect, radius, Icon::Music);
            false
        }
    }
    pub fn trim(&mut self, ctx: &egui::Context) {
        self.receive(ctx);
        while self.retained > TEXTURE_BUDGET || self.entries.len() > MAX_ENTRIES {
            let victim = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(id, _)| id.clone());
            let Some(victim) = victim else { break };
            let entry = self.entries.remove(&victim).unwrap();
            self.retained = self.retained.saturating_sub(entry.bytes);
            // Dropping the last TextureHandle also frees the renderer texture.
        }
    }
}
impl Drop for Artwork {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Channel fields drop without waiting on a slow network. Workers then
        // stop at their next boundary; requests have a hard ten-second timeout.
    }
}

fn valid_artwork_url(url: &str) -> bool {
    [
        "https://i.ytimg.com/",
        "https://yt3.googleusercontent.com/",
        "https://lh3.googleusercontent.com/",
        "https://yt3.ggpht.com/",
    ]
    .iter()
    .any(|host| url.starts_with(host))
}

fn artwork_size(points: f32, pixels_per_point: f32) -> u32 {
    let pixels = points * pixels_per_point;
    if pixels <= 320. { 320 } else { 640 }
}

fn sized_artwork_url(url: &str, size: u32) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.into();
    };
    if !matches!(
        parsed.host_str(),
        Some("yt3.googleusercontent.com" | "lh3.googleusercontent.com" | "yt3.ggpht.com")
    ) {
        return url.into();
    }
    let Some((image, options)) = parsed.path().rsplit_once('=') else {
        return url.into();
    };
    let dimension = |option: &str| {
        option.len() > 1
            && matches!(option.as_bytes()[0], b'w' | b'h' | b's')
            && option.as_bytes()[1..].iter().all(u8::is_ascii_digit)
    };
    if !options.split('-').any(dimension) {
        return url.into();
    }
    let mut path = format!("{image}=w{size}-h{size}");
    for option in options.split('-').filter(|option| !dimension(option)) {
        path.push('-');
        path.push_str(option);
    }
    parsed.set_path(&path);
    parsed.into()
}

fn fetch_image(
    agent: &ureq::Agent,
    dir: Option<&Path>,
    id: &str,
    url: &str,
    size: u32,
) -> Result<ColorImage, String> {
    if !valid_artwork_url(url) {
        return Err("Invalid artwork URL".into());
    }
    // Catalog IDs and URLs never become filesystem paths.
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    url.hash(&mut hasher);
    let path = dir.map(|dir| dir.join(format!("{:016x}.jpg", hasher.finish())));
    if let Some(path) = &path
        && let Ok(file) = fs::File::open(path)
    {
        let mut bytes = Vec::new();
        if file.take(MAX_DOWNLOAD + 1).read_to_end(&mut bytes).is_ok()
            && let Ok(image) = decode(&bytes, size)
        {
            return Ok(image);
        }
    }
    let bytes = agent
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    let image = decode(&bytes, size)?;
    if let (Some(dir), Some(path)) = (dir, path)
        && fs::create_dir_all(dir).is_ok()
    {
        // Distinct names make concurrent downloads safe; this worker owns the
        // video id while it is pending. A partial file simply reloads next time.
        if fs::write(&path, &bytes).is_ok() {
            trim_disk(dir);
        }
    }
    Ok(image)
}
fn decode(bytes: &[u8], size: u32) -> Result<ColorImage, String> {
    if bytes.len() as u64 > MAX_DOWNLOAD {
        return Err("artwork exceeds download limit".into());
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(1024);
    limits.max_image_height = Some(1024);
    limits.max_alloc = Some(4 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| e.to_string())?;
    let rgba = decoded.thumbnail(size, size).to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    ))
}
fn trim_disk(dir: &Path) {
    let Ok(files) = fs::read_dir(dir) else { return };
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = files
        .flatten()
        .filter_map(|file| {
            let metadata = file.metadata().ok()?;
            (metadata.is_file() && file.path().extension().is_some_and(|ext| ext == "jpg")).then(
                || {
                    (
                        file.path(),
                        metadata.len(),
                        metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
                    )
                },
            )
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, bytes, _)| bytes).sum();
    if total > DISK_BUDGET {
        files.sort_by_key(|(_, _, modified)| *modified);
        for (path, bytes, _) in files {
            if total <= DISK_BUDGET {
                break;
            }
            if fs::remove_file(path).is_ok() {
                total = total.saturating_sub(bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn artwork_decoding_limits_dimensions_and_preserves_aspect_ratio() {
        let image = decode(&png(800, 600), 320).unwrap();
        assert_eq!(image.size, [320, 240]);
        let large = decode(&png(800, 600), 640).unwrap();
        assert_eq!(large.size, [640, 480]);
        assert_eq!(decode(&png(640, 640), 640).unwrap().size, [640, 640]);
        assert!(decode(&png(1025, 1), 640).is_err());
        assert!(decode(b"not an image", 640).is_err());
        assert!(decode(&vec![0; MAX_DOWNLOAD as usize + 1], 640).is_err());
    }

    #[test]
    fn album_artwork_requests_account_for_display_scale_and_preserve_url_options() {
        assert_eq!(artwork_size(64., 2.), 320);
        assert_eq!(artwork_size(220., 1.), 320);
        assert_eq!(artwork_size(220., 2.), 640);
        assert_eq!(artwork_size(1000., 2.), 640);
        for host in [
            "yt3.googleusercontent.com",
            "lh3.googleusercontent.com",
            "yt3.ggpht.com",
        ] {
            assert_eq!(
                sized_artwork_url(&format!("https://{host}/cover=w120-h120-l90-rj"), 640),
                format!("https://{host}/cover=w640-h640-l90-rj")
            );
            assert_eq!(
                sized_artwork_url(&format!("https://{host}/avatar=s88-c-k?token=test"), 320),
                format!("https://{host}/avatar=w320-h320-c-k?token=test")
            );
        }
        for url in [
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg",
            "https://yt3.googleusercontent.com/cover",
            "https://yt3.googleusercontent.com/cover=unknown",
            "https://example.com/cover=w120-h120",
        ] {
            assert_eq!(sized_artwork_url(url, 640), url);
        }
    }

    #[test]
    fn large_album_artwork_does_not_reuse_a_small_cached_texture() {
        let ctx = egui::Context::default();
        let (jobs, requests) = mpsc::sync_channel(8);
        let (done, results) = mpsc::sync_channel(4);
        let mut cache = Artwork {
            hero: None,
            entries: HashMap::new(),
            clock: 0,
            next_ticket: 0,
            retained: 0,
            jobs,
            results,
            shutdown: Arc::new(AtomicBool::new(false)),
        };
        let url = "https://yt3.googleusercontent.com/cover=w120-h120-l90-rj";
        let render = |cache: &mut Artwork, edge| {
            let mut ready = false;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(800., 800.),
                    )),
                    ..Default::default()
                },
                |ui| {
                    ready = cache.paint_url(
                        ui,
                        "album",
                        url,
                        Rect::from_min_size(pos2(20., 20.), egui::vec2(edge, edge)),
                        8,
                    );
                },
            );
            output.textures_delta.clear();
            ready
        };
        assert!(!render(&mut cache, 64.));
        let small = requests.try_recv().unwrap();
        assert_eq!(small.size, 320);
        done.send(Loaded {
            id: small.id.clone(),
            ticket: small.ticket,
            image: Some(ColorImage::filled([320, 320], egui::Color32::BLACK)),
        })
        .unwrap();
        assert!(render(&mut cache, 64.));
        assert!(requests.try_recv().is_err());
        assert!(!render(&mut cache, 440.));
        let large = requests.try_recv().unwrap();
        assert_eq!(large.size, 640);
        assert_ne!(large.id, small.id);
        assert_ne!(large.url, small.url);
        done.send(Loaded {
            id: large.id.clone(),
            ticket: large.ticket,
            image: Some(ColorImage::filled([640, 640], egui::Color32::BLACK)),
        })
        .unwrap();
        assert!(render(&mut cache, 440.));
        assert!(render(&mut cache, 64.));
        assert!(requests.try_recv().is_err());
        assert_eq!(cache.retained, (320 * 320 + 640 * 640) * 4);
    }

    #[test]
    fn eviction_releases_textures_and_keeps_recently_used_artwork() {
        let ctx = egui::Context::default();
        let mut cache = Artwork::default();
        for i in 0..50 {
            let image = ColorImage::filled([320, 180], egui::Color32::BLACK);
            let bytes = image.pixels.len() * 4;
            let texture = ctx.load_texture(format!("test-{i}"), image, TextureOptions::LINEAR);
            cache.retained += bytes;
            cache.entries.insert(
                i.to_string(),
                Entry {
                    state: State::Ready(texture),
                    used: i,
                    ticket: i,
                    bytes,
                },
            );
        }
        cache.entries.get_mut("0").unwrap().used = 100;
        let before = ctx.tex_manager().read().num_allocated();
        cache.trim(&ctx);
        assert!(cache.retained <= TEXTURE_BUDGET);
        assert!(cache.entries.contains_key("0"));
        assert!(cache.entries.contains_key("49"));
        assert!(!cache.entries.contains_key("1"));
        assert!(ctx.tex_manager().read().num_allocated() < before);
        assert!(matches!(&cache.entries["0"].state, State::Ready(_)));
        ctx.tex_manager().write().take_delta().clear();
    }
}

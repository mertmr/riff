//! Opt-in visual fixtures and captures. This module is excluded from normal builds.
use crate::{
    app::{Action, MusicApp, Playback, Recommendations, View},
    model::{Collection, CollectionKind, Playlist, SavedAlbum, Track},
    radio::Radio,
};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
static START: OnceLock<Instant> = OnceLock::new();
static FIRST_UI_MS: OnceLock<f64> = OnceLock::new();
static REPORTED: AtomicBool = AtomicBool::new(false);

pub fn start_timer() {
    let _ = START.set(Instant::now());
}

pub fn finish_first_ui(ctx: &egui::Context) {
    if std::env::var_os("RIFF_QA_STARTUP").is_some()
        && FIRST_UI_MS.get().is_none()
        && let Some(start) = START.get()
    {
        let _ = FIRST_UI_MS.set(start.elapsed().as_secs_f64() * 1000.0);
        // The following logic call occurs after eframe has painted, presented,
        // and shown the first native frame. No screenshot readback is needed.
        ctx.request_repaint();
    }
}

pub fn report_startup(frame: &eframe::Frame) {
    if std::env::var_os("RIFF_QA_STARTUP").is_some()
        && frame.info().cpu_usage.is_some()
        && let Some(first_ui_ms) = FIRST_UI_MS.get()
        && !REPORTED.swap(true, Ordering::Relaxed)
        && let Some(start) = START.get()
    {
        println!(
            "{}",
            serde_json::json!({"first_ui_ms": first_ui_ms,
                              "first_frame_ms": start.elapsed().as_secs_f64() * 1000.0})
        );
    }
}
pub struct Capture {
    path: Option<PathBuf>,
    start: Instant,
    requested: bool,
}
pub fn window_size() -> [f32; 2] {
    std::env::var("RIFF_SIZE")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some([w.parse().ok()?, h.parse().ok()?])
        })
        .unwrap_or([1280., 800.])
}
pub fn configure(app: &mut MusicApp) {
    let path = std::env::var_os("RIFF_CAPTURE").map(PathBuf::from);
    if path.is_none() && std::env::var_os("RIFF_QA_ARTWORK").is_none() {
        return;
    }
    app.write_library = false;
    app.saved_tracks.clear();
    app.albums.clear();
    app.artists.clear();
    app.playlists.clear();
    app.qa = Some(Capture {
        path,
        start: Instant::now(),
        requested: false,
    });
    // Real catalog identities, isolated from the user's collections and all disk writes.
    if std::env::var("RIFF_FIXTURES").is_ok_and(|v| v == "1") {
        let tracks=[
            ("jfKfPfyJRdk","lofi hip hop radio — beats to relax / study to","Lofi Girl",None),
            ("4NRXx6U8ABQ","Blinding Lights","The Weeknd",Some(200)),
            ("fHI8X4OXluQ","Blinding Lights — Official Audio","The Weeknd",Some(200)),
            ("kJQP7kiw5Fk","Despacito (feat. Daddy Yankee)","Luis Fonsi",Some(282)),
            ("hTWKbfoikeg","Smells Like Teen Spirit","Nirvana",Some(301)),
            ("dQw4w9WgXcQ","Never Gonna Give You Up","Rick Astley",Some(213)),
            ("JGwWNGJdvx8","Shape of You","Ed Sheeran",Some(263)),
            ("RgKAFK5djSk","See You Again (feat. Charlie Puth) — a very long song title that should gracefully truncate at every window size","Wiz Khalifa and an unusually long list of guest artists",Some(237)),
            ("00000000000","Missing artwork and unavailable duration","Unknown artist",None),
        ].into_iter().map(|(id,title,artist,duration_seconds)| Track {id:id.into(),title:title.into(),artist:artist.into(),duration_seconds,..Default::default()}).collect::<Vec<_>>();
        app.favorites = tracks.iter().skip(1).take(6).cloned().collect();
        app.recent = tracks.clone();
        app.results = tracks.clone();
        let album = Collection {
            id: "MPREalbum-fixture".into(),
            title: "After Hours".into(),
            subtitle: "The Weeknd · 2020".into(),
            kind: CollectionKind::Album,
            thumbnail: None,
            params: None,
        };
        let artist = Collection {
            id: "UCartist-fixture".into(),
            title: "The Weeknd".into(),
            subtitle: "Artist".into(),
            kind: CollectionKind::Artist,
            thumbnail: None,
            params: None,
        };
        app.albums.push(SavedAlbum {
            collection: album.clone(),
            tracks: tracks.iter().skip(1).take(2).cloned().collect(),
        });
        app.artists.push(artist.clone());
        app.playlists.push(Playlist {
            id: 1,
            title: "Evening favorites".into(),
            tracks: tracks.iter().skip(1).cloned().collect(),
        });
        app.search_collections = vec![
            album,
            artist,
            Collection {
                id: "VLplaylist-fixture".into(),
                title: "Late night music".into(),
                subtitle: "YouTube Music · 50 songs".into(),
                kind: CollectionKind::Playlist,
                thumbnail: None,
                params: None,
            },
        ];
        app.queue = tracks.iter().skip(2).cloned().collect();
        app.current = Some(tracks[1].clone());
        app.playback = Playback::Paused;
        app.position = 48.;
        app.duration = Some(200.);
        app.seekable = true;
        app.submitted_query = "The Weeknd".into();
        app.query = "The Weeknd".into();
        app.recommendations = Recommendations {
            seed: Some(tracks[1].clone()),
            tracks: tracks.iter().skip(3).cloned().collect(),
            ..Default::default()
        };
    } else {
        app.favorites.clear();
        app.recent.clear();
        app.queue.clear();
    }
    match std::env::var("RIFF_VIEW").as_deref() {
        Ok("home-loading") => {
            app.recommendations.tracks.clear();
            app.recommendations.searching = true;
        }
        Ok("home-error") => {
            app.recommendations.tracks.clear();
            app.recommendations.error = Some("Check your connection and try again.".into());
        }
        Ok("home-empty") => app.recommendations.tracks.clear(),
        Ok("home-long") => {
            app.recommendations.seed = app.results.get(7).cloned();
            app.queue_open = true;
        }
        Ok("radio" | "radio-loading" | "radio-error") => {
            app.view = View::Search;
            app.queue_open = true;
            app.queue = app.results.iter().skip(2).take(1).cloned().collect();
            if let Some(seed) = app.current.clone() {
                let mut radio = Radio::new(seed);
                radio.extend(app.results.iter().skip(3).cloned().collect(), &app.queue);
                match std::env::var("RIFF_VIEW").as_deref() {
                    Ok("radio-loading") => {
                        radio.searching = true;
                        radio.upcoming.clear();
                    }
                    Ok("radio-error") => {
                        radio.error = Some("Couldn't load song radio. Check your connection or update yt-dlp, then retry.".into());
                        radio.upcoming.clear();
                    }
                    _ => {}
                }
                app.radio = Some(radio);
            }
        }
        Ok("search") => app.view = View::Search,
        Ok("library") => app.view = View::Library,
        Ok("albums") => app.view = View::Albums,
        Ok("artists") => app.view = View::Artists,
        Ok("playlists") => app.view = View::Playlists,
        Ok("playlist" | "playlist-editor") => {
            app.view = View::Collection;
            app.playlist_id = app.playlists.first().map(|p| p.id);
            if std::env::var("RIFF_VIEW").as_deref() == Ok("playlist-editor") {
                app.playlist_editor = Some(crate::app::PlaylistEditor::Create {
                    title: "My new playlist".into(),
                    tracks: vec![],
                });
            }
        }
        Ok("album") => {
            app.view = View::Collection;
            if let Some(album) = app.albums.first() {
                app.collection = Some(album.collection.clone());
                app.detail.tracks = album.tracks.clone();
            }
        }
        Ok("artist") => {
            app.view = View::Collection;
            app.collection = app.artists.first().cloned();
            app.detail.tracks = app.results.iter().skip(1).take(2).cloned().collect();
            app.detail.collections = app.albums.iter().map(|a| a.collection.clone()).collect();
        }
        Ok("favorites") => app.view = View::Favorites,
        Ok("queue") => {
            app.view = View::Search;
            app.queue_open = true;
            app.queue_selected = Some(1);
        }
        Ok("long") => {
            app.view = View::Search;
            app.current = app.results.get(7).cloned();
            app.selected = Some(7);
            app.reveal_selection = true;
        }
        Ok("missing") => {
            app.view = View::Search;
            app.current = app.results.last().cloned();
            app.duration = None;
            app.seekable = false;
            app.playback = Playback::Stopped;
            app.selected = Some(8);
            app.reveal_selection = true;
        }
        Ok("loading") => {
            app.view = View::Search;
            app.searching = true;
            app.playback = Playback::Loading;
        }
        Ok("error") => {
            app.view = View::Search;
            app.search_error = Some(
                "Couldn't reach YouTube. Check your connection or update yt-dlp, then retry."
                    .into(),
            );
            app.playback = Playback::Error;
            app.playback_error =
                Some("This track is unavailable. Try another track or update yt-dlp.".into());
        }
        _ => app.view = View::Home,
    }
    let selected = app.selected;
    app.dispatch(Action::RefreshLibrary, &egui::Context::default());
    app.selected = selected;
}
pub fn capture(app: &mut MusicApp, ctx: &egui::Context) {
    let Some(qa) = &mut app.qa else {
        return;
    };
    let Some(path) = &qa.path else {
        // The performance scenario must upload real covers, rather than pass
        // its memory budget while only rendering empty placeholders.
        if !qa.requested && !app.artwork.is_loading() && app.artwork.loaded_count() >= 6 {
            qa.requested = true;
            println!(
                "{}",
                serde_json::json!({"artwork_loaded": app.artwork.loaded_count()})
            );
        }
        return;
    };
    if !qa.requested {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    let image = ctx.input(|i| {
        i.events.iter().find_map(|event| {
            if let egui::Event::Screenshot { image, .. } = event {
                Some(image.clone())
            } else {
                None
            }
        })
    });
    if let Some(image) = image {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
        image::save_buffer(
            path,
            &pixels,
            image.width() as u32,
            image.height() as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();
        println!(
            "Captured {} ({} × {})",
            path.display(),
            image.width(),
            image.height()
        );
        if std::env::var("RIFF_QA_HOLD").is_err() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    } else if !qa.requested
        && qa.start.elapsed() > Duration::from_secs(3)
        && (!app.artwork.is_loading() || qa.start.elapsed() > Duration::from_secs(15))
    {
        qa.requested = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
}

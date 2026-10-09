//! Application state and effects. Rendering emits actions; workers never touch UI state.
use crate::{
    model::Track,
    player::{self, PlaybackEvent, PlayerCommand, PlayerEvent},
    radio::Radio,
    search,
    storage::{self, LibraryWriter, Session},
    ui,
};
use eframe::egui;
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum View {
    Home,
    Search,
    Library,
    Favorites,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Playback {
    Empty,
    Loading,
    Playing,
    Paused,
    Stopped,
    Error,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LibraryTab {
    All,
    Favorites,
    Recent,
}

pub enum Action {
    Navigate(View),
    Search,
    CancelSearch,
    FocusSearch,
    Select(usize),
    Play(Track),
    StartRadio(Track),
    StopRadio,
    RetryRadio,
    RefreshRecommendations,
    Discover(&'static str),
    TogglePlayback,
    Previous,
    Next,
    Favorite(Track),
    Enqueue(Track, bool),
    QueuePlay(usize),
    QueueRemove(usize),
    QueueMove(usize, isize),
    QueueSelect(usize),
    ClearQueue,
    ToggleQueue,
    Stop,
    Volume(u8),
    Mute,
    Seek(f64),
    PlayFavorites,
    RefreshLibrary,
    LibraryTab(LibraryTab),
    DismissError,
    DismissNotice,
}
struct SearchResult {
    token: u64,
    result: Result<Vec<Track>, String>,
}
struct SearchJob {
    token: u64,
    query: String,
    ctx: egui::Context,
}
pub struct Notice {
    pub text: String,
    pub until: Instant,
}

/// Session-only suggestions from a public YouTube mix, separate from song radio.
#[derive(Default)]
pub struct Recommendations {
    pub seed: Option<Track>,
    pub tracks: Vec<Track>,
    pub searching: bool,
    pub error: Option<String>,
}

impl Recommendations {
    fn complete(&mut self, result: Result<Vec<Track>, String>, known: &[Track]) {
        self.searching = false;
        self.error = None;
        self.tracks.clear();
        match result {
            Ok(tracks) => {
                let mut seen: HashSet<&str> = known.iter().map(|t| t.id.as_str()).collect();
                if let Some(seed) = &self.seed {
                    seen.insert(&seed.id);
                }
                self.tracks = tracks
                    .iter()
                    .filter(|t| seen.insert(&t.id))
                    .take(12)
                    .cloned()
                    .collect();
            }
            Err(error) => self.error = Some(error),
        }
    }
}

pub struct MusicApp {
    pub query: String,
    pub submitted_query: String,
    pub results: Vec<Track>,
    pub favorites: Vec<Track>,
    pub recent: Vec<Track>,
    pub library: Vec<Track>,
    pub filter: String,
    pub library_tab: LibraryTab,
    pub queue: VecDeque<Track>,
    pub radio: Option<Radio>,
    pub recommendations: Recommendations,
    pub current: Option<Track>,
    history: Vec<Track>,
    pub view: View,
    pub selected: Option<usize>,
    pub reveal_selection: bool,
    pub write_library: bool,
    pub queue_selected: Option<usize>,
    pub queue_open: bool,
    pub searching: bool,
    pub search_error: Option<String>,
    pub search_done: bool,
    pub playback: Playback,
    pub buffering: bool,
    pub position: f64,
    pub duration: Option<f64>,
    pub seekable: bool,
    pub volume: u8,
    unmuted_volume: u8,
    pub playback_error: Option<String>,
    pub global_error: Option<String>,
    pub notice: Option<Notice>,
    pub focus_search: bool,
    pub artwork: ui::widgets::Artwork,
    search_token: u64,
    search_epoch: Arc<AtomicU64>,
    search_tx: Sender<SearchJob>,
    search_rx: Receiver<SearchResult>,
    radio_token: u64,
    radio_epoch: Arc<AtomicU64>,
    radio_tx: Sender<SearchJob>,
    radio_rx: Receiver<SearchResult>,
    recommendation_token: u64,
    recommendation_epoch: Arc<AtomicU64>,
    recommendation_tx: Sender<SearchJob>,
    recommendation_rx: Receiver<SearchResult>,
    worker_ctx: egui::Context,
    generation: u64,
    player_tx: Sender<PlayerCommand>,
    player_rx: Receiver<PlayerEvent>,
    player_worker: Option<thread::JoinHandle<()>>,
    writer: LibraryWriter,
    #[cfg(feature = "ui-qa")]
    pub qa: Option<crate::qa::Capture>,
}
impl MusicApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::from_context(&cc.egui_ctx)
    }
    fn from_context(ctx: &egui::Context) -> Self {
        ui::theme::install(ctx);
        let (favorites, session, global_error) = storage::load_library();
        let (search_tx, jobs) = mpsc::channel::<SearchJob>();
        let (result_tx, search_rx) = mpsc::channel();
        let search_epoch = Arc::new(AtomicU64::new(0));
        let cancellation = search_epoch.clone();
        thread::spawn(move || {
            while let Ok(mut job) = jobs.recv() {
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                let result = search::search_youtube(&job.query, || {
                    cancellation.load(Ordering::Relaxed) != job.token
                });
                let _ = result_tx.send(SearchResult {
                    token: job.token,
                    result,
                });
                job.ctx.request_repaint();
            }
        });
        let repaint = ctx.clone();
        let (radio_tx, jobs) = mpsc::channel::<SearchJob>();
        let (result_tx, radio_rx) = mpsc::channel();
        let radio_epoch = Arc::new(AtomicU64::new(0));
        let cancellation = radio_epoch.clone();
        thread::spawn(move || {
            while let Ok(mut job) = jobs.recv() {
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                let result = search::radio_youtube(&job.query, || {
                    cancellation.load(Ordering::Relaxed) != job.token
                });
                let _ = result_tx.send(SearchResult {
                    token: job.token,
                    result,
                });
                job.ctx.request_repaint();
            }
        });
        let (player_tx, player_rx, worker) =
            player::start_player(move || repaint.request_repaint());
        let (recommendation_tx, jobs) = mpsc::channel::<SearchJob>();
        let (result_tx, recommendation_rx) = mpsc::channel();
        let recommendation_epoch = Arc::new(AtomicU64::new(0));
        let cancellation = recommendation_epoch.clone();
        thread::spawn(move || {
            while let Ok(mut job) = jobs.recv() {
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                let result = search::radio_youtube(&job.query, || {
                    cancellation.load(Ordering::Relaxed) != job.token
                });
                let _ = result_tx.send(SearchResult {
                    token: job.token,
                    result,
                });
                job.ctx.request_repaint();
            }
        });
        let mut app = Self {
            query: String::new(),
            submitted_query: String::new(),
            results: Vec::new(),
            favorites,
            recent: session.recent,
            library: Vec::new(),
            filter: String::new(),
            library_tab: LibraryTab::All,
            queue: session.queue.into(),
            radio: None,
            recommendations: Recommendations::default(),
            current: None,
            history: Vec::new(),
            view: View::Home,
            selected: None,
            reveal_selection: false,
            write_library: true,
            queue_selected: None,
            queue_open: false,
            searching: false,
            search_error: None,
            search_done: false,
            playback: Playback::Empty,
            buffering: false,
            position: 0.,
            duration: None,
            seekable: false,
            volume: session.volume,
            unmuted_volume: session.volume.max(1),
            playback_error: None,
            global_error,
            notice: None,
            focus_search: false,
            artwork: Default::default(),
            search_token: 0,
            search_epoch,
            search_tx,
            search_rx,
            radio_token: 0,
            radio_epoch,
            radio_tx,
            radio_rx,
            recommendation_token: 0,
            recommendation_epoch,
            recommendation_tx,
            recommendation_rx,
            worker_ctx: ctx.clone(),
            generation: 0,
            player_tx,
            player_rx,
            player_worker: Some(worker),
            writer: LibraryWriter::new(ctx.clone()),
            #[cfg(feature = "ui-qa")]
            qa: None,
        };
        app.rebuild_library();
        let _ = app.player_tx.send(PlayerCommand::SetVolume(app.volume));
        #[cfg(feature = "ui-qa")]
        crate::qa::configure(&mut app);
        app
    }
    pub fn is_favorite(&self, id: &str) -> bool {
        self.favorites.iter().any(|t| t.id == id)
    }
    pub fn active_tracks(&self) -> &[Track] {
        match self.view {
            View::Search => &self.results,
            View::Favorites | View::Library => &self.library,
            View::Home if !self.recommendations.tracks.is_empty() => &self.recommendations.tracks,
            View::Home => &self.recent[..self.recent.len().min(12)],
        }
    }
    fn persist(&self) {
        if !self.write_library {
            return;
        }
        self.writer.save(
            &self.favorites,
            Session {
                recent: self.recent.clone(),
                queue: self.queue.iter().cloned().collect(),
                volume: self.volume,
            },
        );
    }
    fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            until: Instant::now() + Duration::from_secs(4),
        });
    }
    fn rebuild_library(&mut self) {
        self.library.clear();
        let favorites_only =
            self.view == View::Favorites || self.library_tab == LibraryTab::Favorites;
        if self.library_tab != LibraryTab::Recent || self.view == View::Favorites {
            self.library.extend(self.favorites.iter().cloned());
        }
        if !favorites_only {
            for track in &self.recent {
                if !self.library.iter().any(|t| t.id == track.id) {
                    self.library.push(track.clone());
                }
            }
        }
        if !self.filter.trim().is_empty() {
            let needle = self.filter.trim().to_lowercase();
            self.library.retain(|t| {
                t.title.to_lowercase().contains(&needle)
                    || t.artist.to_lowercase().contains(&needle)
            });
        }
        self.selected = None;
    }
    fn play(&mut self, track: Track, add_history: bool) {
        if let Some(radio) = &mut self.radio {
            radio.waiting = false;
            radio.remember(&track.id);
            radio.upcoming.retain(|t| t.id != track.id);
        }
        if add_history
            && let Some(current) = &self.current
            && current.id != track.id
        {
            self.history.push(current.clone());
            if self.history.len() > 100 {
                self.history.remove(0);
            }
        }
        self.generation = self.generation.wrapping_add(1);
        self.current = Some(track.clone());
        self.playback = Playback::Loading;
        self.buffering = false;
        self.position = 0.;
        self.duration = None;
        self.seekable = false;
        self.playback_error = None;
        if self
            .player_tx
            .send(PlayerCommand::Play {
                track,
                generation: self.generation,
            })
            .is_err()
        {
            self.playback = Playback::Error;
            self.playback_error =
                Some("The audio worker stopped. Restart Riff to reconnect.".into());
        }
    }
    fn next(&mut self) {
        let track = self.queue.pop_front().or_else(|| {
            let radio = self.radio.as_mut()?;
            let track = radio.upcoming.pop_front()?;
            radio.continuation = track.clone();
            Some(track)
        });
        if let Some(track) = track {
            self.play(track, true);
            self.queue_selected = None;
            self.persist();
        } else {
            self.generation = self.generation.wrapping_add(1);
            let _ = self.player_tx.send(PlayerCommand::Stop);
            self.playback = Playback::Stopped;
            self.buffering = false;
            if let Some(radio) = &mut self.radio {
                radio.waiting = true;
            }
        }
        self.refill_radio();
    }
    fn stop_radio(&mut self) {
        self.radio_token = self.radio_token.wrapping_add(1);
        self.radio_epoch.store(self.radio_token, Ordering::Relaxed);
        self.radio = None;
        self.queue_selected = None;
    }
    fn recommendation_seed(&self) -> Option<Track> {
        self.recent.first().or(self.favorites.first()).cloned()
    }
    fn refresh_recommendations(&mut self, force: bool) {
        #[cfg(feature = "ui-qa")]
        if self.qa.is_some() {
            return;
        }
        let seed = self.recommendation_seed();
        if !force
            && self.recommendations.seed.as_ref().map(|t| &t.id) == seed.as_ref().map(|t| &t.id)
        {
            return;
        }
        self.recommendation_token = self.recommendation_token.wrapping_add(1);
        self.recommendation_epoch
            .store(self.recommendation_token, Ordering::Relaxed);
        self.recommendations = Recommendations {
            seed: seed.clone(),
            ..Default::default()
        };
        self.selected = None;
        let Some(seed) = seed else { return };
        self.recommendations.searching = true;
        if self
            .recommendation_tx
            .send(SearchJob {
                token: self.recommendation_token,
                query: seed.id,
                ctx: self.worker_ctx.clone(),
            })
            .is_err()
        {
            self.recommendations.searching = false;
            self.recommendations.error =
                Some("Couldn't load recommendations. Restart Riff to reconnect.".into());
        }
    }
    fn refill_radio(&mut self) {
        let Some(radio) = &mut self.radio else {
            return;
        };
        if radio.searching || radio.error.is_some() || radio.upcoming.len() > 3 {
            return;
        }
        radio.searching = true;
        self.radio_token = self.radio_token.wrapping_add(1);
        self.radio_epoch.store(self.radio_token, Ordering::Relaxed);
        if self
            .radio_tx
            .send(SearchJob {
                token: self.radio_token,
                query: radio.continuation.id.clone(),
                ctx: self.worker_ctx.clone(),
            })
            .is_err()
        {
            radio.searching = false;
            radio.error = Some("The radio worker stopped. Restart Riff to reconnect.".into());
        }
    }
    pub fn queue_len(&self) -> usize {
        self.queue.len() + self.radio.as_ref().map_or(0, |r| r.upcoming.len())
    }
    pub fn queue_track(&self, index: usize) -> Option<&Track> {
        if index < self.queue.len() {
            self.queue.get(index)
        } else {
            self.radio.as_ref()?.upcoming.get(index - self.queue.len())
        }
    }
    fn remove_queued(&mut self, index: usize) -> Option<Track> {
        if index < self.queue.len() {
            self.queue.remove(index)
        } else {
            self.radio
                .as_mut()?
                .upcoming
                .remove(index - self.queue.len())
        }
    }
    pub fn can_queue_move(&self, index: usize, delta: isize) -> bool {
        let Some(target) = index.checked_add_signed(delta) else {
            return false;
        };
        index < self.queue_len()
            && target < self.queue_len()
            && (index < self.queue.len()) == (target < self.queue.len())
    }
    fn receive(&mut self) {
        while let Ok(message) = self.recommendation_rx.try_recv() {
            if message.token != self.recommendation_token || self.recommendations.seed.is_none() {
                continue;
            }
            let known: Vec<Track> = self
                .recent
                .iter()
                .chain(&self.favorites)
                .chain(&self.queue)
                .chain(self.current.iter())
                .cloned()
                .collect();
            self.recommendations.complete(message.result, &known);
            self.selected = None;
        }
        while let Ok(message) = self.radio_rx.try_recv() {
            if message.token != self.radio_token {
                continue;
            }
            let Some(radio) = &mut self.radio else {
                continue;
            };
            radio.searching = false;
            match message.result {
                Ok(tracks) => {
                    let before = radio.upcoming.len();
                    radio.extend(tracks, &self.queue);
                    if radio.upcoming.len() == before {
                        radio.error = Some(
                            "No new related songs found. Retry radio or start from another song."
                                .into(),
                        );
                    }
                }
                Err(e) => radio.error = Some(e),
            }
            if radio.waiting && self.queue_len() > 0 {
                self.next();
            }
        }
        while let Ok(message) = self.search_rx.try_recv() {
            if message.token != self.search_token {
                continue;
            }
            self.searching = false;
            self.search_done = true;
            match message.result {
                Ok(tracks) => {
                    self.results = tracks;
                    self.selected = None;
                }
                Err(e) => {
                    self.search_error = Some(e);
                    self.results.clear();
                }
            }
        }
        while let Ok(message) = self.player_rx.try_recv() {
            if message.generation != self.generation {
                continue;
            }
            match message.event {
                PlaybackEvent::Ready => {
                    self.playback = Playback::Playing;
                    if let Some(track) = &self.current {
                        self.recent.retain(|t| t.id != track.id);
                        self.recent.insert(0, track.clone());
                        self.recent.truncate(40);
                        self.rebuild_library();
                        self.persist();
                    }
                }
                PlaybackEvent::Paused(paused) => {
                    if matches!(self.playback, Playback::Playing | Playback::Paused) {
                        self.playback = if paused {
                            Playback::Paused
                        } else {
                            Playback::Playing
                        };
                    }
                }
                PlaybackEvent::Buffering(value) => self.buffering = value,
                PlaybackEvent::Position(value) => self.position = value,
                PlaybackEvent::Duration(value) => self.duration = Some(value),
                PlaybackEvent::Seekable(value) => self.seekable = value,
                PlaybackEvent::Volume(value) => self.volume = value,
                PlaybackEvent::Stopped => {
                    self.playback = Playback::Stopped;
                    self.buffering = false;
                }
                PlaybackEvent::Ended => self.next(),
                PlaybackEvent::Error(e) => {
                    self.playback = Playback::Error;
                    self.playback_error = Some(e);
                    self.buffering = false;
                    self.seekable = false;
                }
                PlaybackEvent::ControlError(e) => self.global_error = Some(e),
            }
        }
        while let Ok(e) = self.writer.errors.try_recv() {
            self.global_error = Some(format!("Couldn't save your library: {e}"));
        }
        if self
            .notice
            .as_ref()
            .is_some_and(|n| Instant::now() > n.until)
        {
            self.notice = None;
        }
    }
    pub fn dispatch(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Navigate(view) => {
                self.view = view;
                self.selected = None;
                self.filter.clear();
                self.library_tab = LibraryTab::All;
                self.rebuild_library();
                if view == View::Search {
                    self.focus_search = true;
                }
                if view == View::Home {
                    self.refresh_recommendations(false);
                }
            }
            Action::FocusSearch => {
                self.view = View::Search;
                self.focus_search = true;
                ctx.memory_mut(|m| m.request_focus(egui::Id::new("global_search")));
            }
            Action::Search => {
                if self.query.trim().is_empty() {
                    self.focus_search = true;
                    return;
                }
                self.submitted_query = self.query.trim().to_owned();
                self.view = View::Search;
                self.search_token = self.search_token.wrapping_add(1);
                self.search_epoch
                    .store(self.search_token, Ordering::Relaxed);
                self.searching = true;
                self.search_error = None;
                self.results.clear();
                self.search_done = false;
                self.selected = None;
                if self
                    .search_tx
                    .send(SearchJob {
                        token: self.search_token,
                        query: self.submitted_query.clone(),
                        ctx: ctx.clone(),
                    })
                    .is_err()
                {
                    self.searching = false;
                    self.search_error =
                        Some("The search worker stopped. Restart Riff to reconnect.".into());
                }
            }
            Action::CancelSearch => {
                self.search_token = self.search_token.wrapping_add(1);
                self.search_epoch
                    .store(self.search_token, Ordering::Relaxed);
                self.searching = false;
            }
            Action::Discover(query) => {
                self.query = query.into();
                self.dispatch(Action::Search, ctx);
            }
            Action::RefreshRecommendations => self.refresh_recommendations(true),
            Action::Select(i) => {
                self.selected = Some(i);
                self.queue_selected = None;
            }
            Action::Play(track) => {
                self.stop_radio();
                self.play(track, true);
            }
            Action::StartRadio(track) => {
                self.stop_radio();
                self.radio = Some(Radio::new(track.clone()));
                if self.current.as_ref().map(|t| &t.id) != Some(&track.id)
                    || !matches!(self.playback, Playback::Playing | Playback::Loading)
                {
                    self.play(track, true);
                }
                self.queue_open = true;
                self.refill_radio();
            }
            Action::StopRadio => {
                self.stop_radio();
                self.notify("Song radio stopped");
            }
            Action::RetryRadio => {
                if let Some(radio) = &mut self.radio {
                    radio.error = None;
                    radio.waiting = self.playback == Playback::Stopped;
                }
                self.refill_radio();
            }
            Action::TogglePlayback => match self.playback {
                Playback::Playing | Playback::Paused => {
                    let _ = self
                        .player_tx
                        .send(PlayerCommand::SetPaused(self.playback != Playback::Paused));
                }
                Playback::Stopped | Playback::Error => {
                    if let Some(t) = self.current.clone() {
                        self.play(t, false);
                    }
                }
                _ => {}
            },
            Action::Previous => {
                if self.position > 3. && self.seekable {
                    let _ = self.player_tx.send(PlayerCommand::Seek(0.));
                } else if let Some(track) = self.history.pop() {
                    if let Some(current) = self.current.clone() {
                        self.queue.push_front(current);
                    }
                    self.play(track, false);
                    self.persist();
                }
            }
            Action::Next => self.next(),
            Action::Stop => {
                self.stop_radio();
                self.generation = self.generation.wrapping_add(1);
                let _ = self.player_tx.send(PlayerCommand::Stop);
                self.playback = Playback::Stopped;
                self.buffering = false;
            }
            Action::Favorite(track) => {
                if self.is_favorite(&track.id) {
                    self.favorites.retain(|t| t.id != track.id);
                    self.notify("Removed from favorites");
                } else {
                    self.favorites.insert(0, track);
                    self.notify("Saved to favorites");
                }
                self.rebuild_library();
                self.persist();
            }
            Action::Enqueue(track, next) => {
                if let Some(radio) = &mut self.radio {
                    radio.upcoming.retain(|t| t.id != track.id);
                    radio.remember(&track.id);
                }
                if next {
                    self.queue.push_front(track);
                } else {
                    self.queue.push_back(track);
                }
                self.notify(if next {
                    "Added to play next"
                } else {
                    "Added to queue"
                });
                self.persist();
            }
            Action::QueuePlay(i) => {
                let from_radio = i >= self.queue.len();
                if let Some(track) = self.remove_queued(i) {
                    if from_radio && let Some(radio) = &mut self.radio {
                        radio.continuation = track.clone();
                    }
                    self.play(track, true);
                    self.queue_selected = None;
                    self.persist();
                    self.refill_radio();
                }
            }
            Action::QueueRemove(i) => {
                self.remove_queued(i);
                self.queue_selected = None;
                self.persist();
                self.refill_radio();
            }
            Action::QueueMove(i, delta) => {
                if self.can_queue_move(i, delta) {
                    let target = i.checked_add_signed(delta).unwrap();
                    if i < self.queue.len() {
                        self.queue.swap(i, target);
                    } else if let Some(radio) = &mut self.radio {
                        radio
                            .upcoming
                            .swap(i - self.queue.len(), target - self.queue.len());
                    }
                    self.queue_selected = Some(target);
                    self.persist();
                }
            }
            Action::QueueSelect(i) => {
                self.queue_selected = Some(i);
                self.selected = None;
            }
            Action::ClearQueue => {
                self.stop_radio();
                self.queue.clear();
                self.queue_selected = None;
                self.persist();
            }
            Action::ToggleQueue => self.queue_open = !self.queue_open,
            Action::Volume(v) => {
                let v = v.min(100);
                self.volume = v;
                if v > 0 {
                    self.unmuted_volume = v;
                }
                let _ = self.player_tx.send(PlayerCommand::SetVolume(v));
                self.persist();
            }
            Action::Mute => self.dispatch(
                Action::Volume(if self.volume == 0 {
                    self.unmuted_volume
                } else {
                    0
                }),
                ctx,
            ),
            Action::Seek(p) => {
                if self.seekable {
                    let _ = self.player_tx.send(PlayerCommand::Seek(p));
                }
            }
            Action::PlayFavorites => {
                self.stop_radio();
                if let Some(first) = self.favorites.first().cloned() {
                    self.queue = self.favorites.iter().skip(1).cloned().collect();
                    self.play(first, true);
                    self.persist();
                }
            }
            Action::RefreshLibrary => self.rebuild_library(),
            Action::LibraryTab(tab) => {
                self.library_tab = tab;
                self.rebuild_library();
            }
            Action::DismissError => self.global_error = None,
            Action::DismissNotice => self.notice = None,
        }
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.dispatch(Action::FocusSearch, ctx);
        }
        if egui::Popup::is_any_open(ctx) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.queue_open = false;
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        if ctx.text_edit_focused() || egui::Popup::is_any_open(ctx) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
            self.dispatch(Action::TogglePlayback, ctx);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::J)) {
            self.queue_open = !self.queue_open;
        }
        if ctx.memory(|m| m.focused()).is_some_and(|focus| {
            ctx.data(|d| d.get_temp::<egui::Id>(egui::Id::new("selected_row_focus"))) != Some(focus)
        }) {
            return;
        }
        if self.queue_open && self.queue_selected.is_some() && self.queue_len() > 0 {
            let mut i = self.queue_selected.unwrap().min(self.queue_len() - 1);
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown))
            {
                i = (i + 1).min(self.queue_len() - 1);
            }
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                i = i.saturating_sub(1);
            }
            self.queue_selected = Some(i);
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                self.dispatch(Action::QueuePlay(i), ctx);
            }
            if ctx.input_mut(|input| {
                input.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    || input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
            }) {
                self.dispatch(Action::QueueRemove(i), ctx);
            }
            return;
        }
        let count = self.active_tracks().len();
        if count > 0 {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(count - 1)));
                self.reveal_selection = true;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                self.selected = Some(self.selected.unwrap_or(0).saturating_sub(1));
                self.reveal_selection = true;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                && let Some(i) = self.selected
                && let Some(track) = self.active_tracks().get(i).cloned()
            {
                self.dispatch(Action::Play(track), ctx);
            }
        }
    }
    pub fn can_previous(&self) -> bool {
        !self.history.is_empty() || (self.position > 3. && self.seekable)
    }
}
impl eframe::App for MusicApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe also calls logic for covered/minimized windows. Drain events
        // and advance the queue there, without rendering or retaining a backlog.
        self.receive();
        #[cfg(feature = "ui-qa")]
        crate::qa::report_startup(_frame);
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // Start only when Home is shown; opt-in captures keep their deterministic fixtures.
        #[cfg(feature = "ui-qa")]
        let allow_recommendations = self.qa.is_none();
        #[cfg(not(feature = "ui-qa"))]
        let allow_recommendations = true;
        if allow_recommendations && self.view == View::Home && self.recommendations.seed.is_none() {
            self.refresh_recommendations(false);
        }
        self.shortcuts(&ctx);
        ui::show(self, ui);
        // Workers wake the UI when something changes. An idle or paused player
        // has no periodic redraw; controls still repaint immediately on input.
        if matches!(self.playback, Playback::Loading)
            || self.searching
            || self.recommendations.searching
            || self.radio.as_ref().is_some_and(|r| r.searching)
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(notice) = &self.notice {
            ctx.request_repaint_after(notice.until.saturating_duration_since(Instant::now()));
        }
        self.artwork.trim(&ctx);
        #[cfg(feature = "ui-qa")]
        crate::qa::capture(self, &ctx);
        #[cfg(feature = "ui-qa")]
        crate::qa::finish_first_ui(&ctx);
    }
}
impl Drop for MusicApp {
    fn drop(&mut self) {
        self.search_epoch.fetch_add(1, Ordering::Relaxed);
        self.radio_epoch.fetch_add(1, Ordering::Relaxed);
        self.recommendation_epoch.fetch_add(1, Ordering::Relaxed);
        let _ = self.player_tx.send(PlayerCommand::Shutdown);
        if let Some(worker) = self.player_worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn track(id: &str) -> Track {
        Track {
            id: id.into(),
            title: id.into(),
            artist: "Artist".into(),
            duration_seconds: Some(180),
        }
    }
    fn fixture() -> (
        MusicApp,
        Receiver<PlayerCommand>,
        Sender<PlayerEvent>,
        egui::Context,
    ) {
        let ctx = egui::Context::default();
        let mut app = MusicApp::from_context(&ctx);
        app.write_library = false;
        app.favorites.clear();
        app.recent.clear();
        app.queue.clear();
        app.recommendations = Recommendations::default();
        app.rebuild_library();
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let (search_tx, search_jobs) = mpsc::channel::<SearchJob>();
        thread::spawn(move || while search_jobs.recv().is_ok() {});
        app.search_tx = search_tx;
        let (recommendation_tx, recommendation_jobs) = mpsc::channel::<SearchJob>();
        thread::spawn(move || while recommendation_jobs.recv().is_ok() {});
        app.recommendation_tx = recommendation_tx;
        app.player_tx = tx;
        app.player_rx = rx;
        (app, commands, events, ctx)
    }
    #[test]
    fn recommendations_filter_known_tracks_and_duplicates_and_bound_results() {
        let seed = track("dQw4w9WgXcQ");
        let known = track("hTWKbfoikeg");
        let mut recommendations = Recommendations {
            seed: Some(seed.clone()),
            searching: true,
            ..Default::default()
        };
        let new: Vec<_> = (0..30).map(|i| track(&format!("{i:011}"))).collect();
        let mut results = vec![seed, known.clone(), new[0].clone()];
        results.extend(new.clone());
        recommendations.complete(Ok(results), &[known]);
        assert_eq!(recommendations.tracks, new[..12]);
        assert!(!recommendations.searching);
        recommendations.complete(Err("offline".into()), &[]);
        assert!(recommendations.tracks.is_empty());
        assert_eq!(recommendations.error.as_deref(), Some("offline"));
        recommendations.complete(Ok(vec![]), &[]);
        assert!(recommendations.error.is_none());
    }
    #[test]
    fn home_recommendations_are_cached_cancel_stale_work_and_do_not_start_playback() {
        let (mut app, commands, _, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.recommendation_tx = tx;
        app.recommendation_rx = rx;
        app.dispatch(Action::Navigate(View::Home), &ctx);
        assert!(jobs.try_recv().is_err()); // No invented seed on first launch.
        let favorite = track("dQw4w9WgXcQ");
        app.favorites.push(favorite.clone());
        app.dispatch(Action::Navigate(View::Home), &ctx);
        let old = jobs.recv().unwrap();
        assert_eq!(old.query, favorite.id);
        app.dispatch(Action::Navigate(View::Home), &ctx);
        assert!(jobs.try_recv().is_err()); // Same seed uses the session cache.
        let recent = track("hTWKbfoikeg");
        app.recent.push(recent.clone());
        app.dispatch(Action::Navigate(View::Home), &ctx);
        let new = jobs.recv().unwrap();
        assert_eq!(new.query, recent.id); // Recent listening takes priority.
        results
            .send(SearchResult {
                token: old.token,
                result: Err("obsolete".into()),
            })
            .unwrap();
        app.receive();
        assert!(app.recommendations.searching);
        assert!(app.recommendations.error.is_none());
        let suggested = track("JGwWNGJdvx8");
        results
            .send(SearchResult {
                token: new.token,
                result: Ok(vec![recent, favorite, suggested.clone()]),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.recommendations.tracks, vec![suggested]);
        assert!(commands.try_recv().is_err());
        assert!(app.queue.is_empty() && app.radio.is_none() && app.current.is_none());
        app.dispatch(Action::RefreshRecommendations, &ctx);
        let retry = jobs.recv().unwrap();
        assert_ne!(retry.token, new.token);
        app.recent.clear();
        app.favorites.clear();
        app.dispatch(Action::Navigate(View::Home), &ctx);
        results
            .send(SearchResult {
                token: retry.token,
                result: Ok(vec![track("4NRXx6U8ABQ")]),
            })
            .unwrap();
        app.receive();
        assert!(app.recommendations.seed.is_none() && app.recommendations.tracks.is_empty());
    }
    #[test]
    fn discovery_shortcuts_submit_search_without_playing_or_editing_queue() {
        let (mut app, commands, _, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        app.search_tx = tx;
        app.dispatch(Action::Discover("chill lofi music"), &ctx);
        assert_eq!(app.view, View::Search);
        assert_eq!(jobs.recv().unwrap().query, "chill lofi music");
        assert_eq!(app.submitted_query, app.query);
        assert!(app.searching && app.queue.is_empty());
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn home_keyboard_navigation_plays_recommendations_and_bounds_recent_selection() {
        let (mut app, commands, _, ctx) = fixture();
        let suggested = track("JGwWNGJdvx8");
        app.recent = vec![track("dQw4w9WgXcQ")];
        app.recommendations.tracks = vec![suggested.clone()];
        for key in [egui::Key::ArrowDown, egui::Key::Enter] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1024., 640.),
                    )),
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    app.shortcuts(ui.ctx());
                    crate::ui::show(&mut app, ui);
                },
            );
            output.textures_delta.clear();
        }
        assert_eq!(app.current, Some(suggested.clone()));
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::Play { track, .. } if track == suggested)
        );
        app.recommendations.tracks.clear();
        app.recent = (0..40).map(|i| track(&format!("{i:011}"))).collect();
        assert_eq!(app.active_tracks().len(), 12);
    }
    #[test]
    fn radio_preserves_manual_queue_filters_duplicates_and_continues() {
        let (mut app, commands, events, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.radio_tx = tx;
        app.radio_rx = rx;
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.dispatch(Action::Enqueue(b.clone(), false), &ctx);
        app.dispatch(Action::StartRadio(a.clone()), &ctx);
        commands.recv().unwrap();
        let job = jobs.recv().unwrap();
        assert_eq!(job.query, a.id);
        results
            .send(SearchResult {
                token: job.token,
                result: Ok(vec![a.clone(), b.clone(), c.clone(), c.clone()]),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.queue, VecDeque::from([b.clone()]));
        assert_eq!(
            app.radio.as_ref().unwrap().upcoming,
            VecDeque::from([c.clone()])
        );
        assert!(app.queue_open);
        // Start from the same playing seed without restarting it.
        app.playback = Playback::Playing;
        app.dispatch(Action::StartRadio(a), &ctx);
        assert!(commands.try_recv().is_err());
        let job = jobs.recv().unwrap();
        results
            .send(SearchResult {
                token: job.token,
                result: Ok(vec![c.clone()]),
            })
            .unwrap();
        app.receive();
        for expected in [b, c] {
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event: PlaybackEvent::Ended,
                })
                .unwrap();
            let mut frame = eframe::Frame::_new_kittest();
            eframe::App::logic(&mut app, &ctx, &mut frame);
            assert_eq!(app.current, Some(expected));
            assert!(matches!(
                commands.recv().unwrap(),
                PlayerCommand::Play { .. }
            ));
        }
        assert!(app.queue.is_empty());
        assert!(app.radio.is_some());
        // Manual queue entries must not change the radio's recommendation seed.
        assert_eq!(jobs.recv().unwrap().query, "dQw4w9WgXcQ");
    }
    #[test]
    fn radio_waits_for_results_and_stop_prevents_late_autoplay() {
        let (mut app, commands, events, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.radio_tx = tx;
        app.radio_rx = rx;
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        app.dispatch(Action::StartRadio(a), &ctx);
        commands.recv().unwrap();
        let job = jobs.recv().unwrap();
        let ended_generation = app.generation;
        events
            .send(PlayerEvent {
                generation: app.generation,
                event: PlaybackEvent::Ended,
            })
            .unwrap();
        app.receive();
        assert!(matches!(commands.recv().unwrap(), PlayerCommand::Stop));
        assert!(app.radio.as_ref().unwrap().waiting);
        events
            .send(PlayerEvent {
                generation: ended_generation,
                event: PlaybackEvent::Ready,
            })
            .unwrap();
        app.receive();
        assert!(app.playback == Playback::Stopped);
        results
            .send(SearchResult {
                token: job.token,
                result: Ok(vec![b.clone()]),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.current, Some(b.clone()));
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::Play { .. }
        ));
        let refill = jobs.recv().unwrap();
        app.dispatch(Action::StopRadio, &ctx);
        assert_eq!(app.current, Some(b));
        results
            .send(SearchResult {
                token: refill.token,
                result: Ok(vec![track("JGwWNGJdvx8")]),
            })
            .unwrap();
        app.receive();
        assert!(app.radio.is_none());
        assert_eq!(app.queue_len(), 0);
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn obsolete_radio_results_and_direct_play_cannot_change_new_session() {
        let (mut app, commands, _, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.radio_tx = tx;
        app.radio_rx = rx;
        app.dispatch(Action::StartRadio(track("dQw4w9WgXcQ")), &ctx);
        commands.recv().unwrap();
        let old = jobs.recv().unwrap();
        let b = track("hTWKbfoikeg");
        app.dispatch(Action::StartRadio(b.clone()), &ctx);
        commands.recv().unwrap();
        let new = jobs.recv().unwrap();
        assert_ne!(old.token, new.token);
        results
            .send(SearchResult {
                token: old.token,
                result: Err("obsolete".into()),
            })
            .unwrap();
        app.receive();
        let radio = app.radio.as_ref().unwrap();
        assert_eq!(radio.seed, b);
        assert!(radio.searching && radio.error.is_none());
        app.dispatch(Action::Play(track("JGwWNGJdvx8")), &ctx);
        commands.recv().unwrap();
        results
            .send(SearchResult {
                token: new.token,
                result: Ok(vec![b]),
            })
            .unwrap();
        app.receive();
        assert!(app.radio.is_none());
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn exhausted_radio_requires_retry_and_queue_edits_respect_priority() {
        let (mut app, commands, _, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.radio_tx = tx;
        app.radio_rx = rx;
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.dispatch(Action::StartRadio(a.clone()), &ctx);
        commands.recv().unwrap();
        let job = jobs.recv().unwrap();
        results
            .send(SearchResult {
                token: job.token,
                result: Ok(vec![a]),
            })
            .unwrap();
        app.receive();
        assert!(app.radio.as_ref().unwrap().error.is_some());
        app.refill_radio();
        assert!(jobs.try_recv().is_err());
        app.dispatch(Action::RetryRadio, &ctx);
        let job = jobs.recv().unwrap();
        results
            .send(SearchResult {
                token: job.token,
                result: Ok(vec![b.clone(), c.clone()]),
            })
            .unwrap();
        app.receive();
        app.dispatch(Action::Enqueue(b.clone(), true), &ctx);
        assert_eq!(app.queue, VecDeque::from([b.clone()]));
        assert_eq!(app.radio.as_ref().unwrap().upcoming, VecDeque::from([c]));
        assert!(!app.can_queue_move(0, 1));
        assert!(!app.can_queue_move(1, -1));
        app.dispatch(Action::StopRadio, &ctx);
        assert_eq!(app.queue, VecDeque::from([b]));
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn queue_edits_keep_the_current_track_and_navigation_keeps_playback() {
        let (mut app, commands, _, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.dispatch(Action::Play(a.clone()), &ctx);
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::Play { .. }
        ));
        app.dispatch(Action::Enqueue(b.clone(), false), &ctx);
        app.dispatch(Action::Enqueue(c.clone(), false), &ctx);
        app.dispatch(Action::QueueMove(1, -1), &ctx);
        assert_eq!(app.queue[0], c);
        app.dispatch(Action::Navigate(View::Favorites), &ctx);
        assert_eq!(app.current, Some(a));
        assert!(app.playback == Playback::Loading);
        app.dispatch(Action::QueuePlay(1), &ctx);
        assert_eq!(app.current, Some(b));
        assert_eq!(app.queue.len(), 1);
        app.dispatch(Action::QueueRemove(0), &ctx);
        assert!(app.queue.is_empty());
    }
    #[test]
    fn hidden_window_drains_playback_events_and_advances_the_queue() {
        let (mut app, commands, events, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        app.dispatch(Action::Play(a), &ctx);
        commands.recv().unwrap();
        app.dispatch(Action::Enqueue(b.clone(), false), &ctx);
        for event in [
            PlaybackEvent::Ready,
            PlaybackEvent::Position(179.),
            PlaybackEvent::Ended,
        ] {
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event,
                })
                .unwrap();
        }
        let mut frame = eframe::Frame::_new_kittest();
        // Match eframe's hidden-window path: logic runs, no UI is constructed.
        eframe::App::logic(&mut app, &ctx, &mut frame);
        assert_eq!(app.current, Some(b));
        assert!(app.queue.is_empty());
        assert!(app.player_rx.try_recv().is_err());
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::Play { .. }
        ));
    }
    #[test]
    fn stale_events_cannot_replace_playback_and_recent_requires_ready() {
        let (mut app, _commands, events, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        app.dispatch(Action::Play(a), &ctx);
        let old = app.generation;
        app.dispatch(Action::Play(b.clone()), &ctx);
        events
            .send(PlayerEvent {
                generation: old,
                event: PlaybackEvent::Error("stale".into()),
            })
            .unwrap();
        app.receive();
        assert!(app.playback == Playback::Loading);
        assert!(app.recent.is_empty());
        events
            .send(PlayerEvent {
                generation: app.generation,
                event: PlaybackEvent::Ready,
            })
            .unwrap();
        app.receive();
        assert!(app.playback == Playback::Playing);
        assert_eq!(app.recent, vec![b]);
    }
    #[test]
    fn favorites_and_recent_build_a_unique_filterable_library() {
        let (mut app, _commands, _, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        app.recent = vec![a.clone(), b.clone()];
        app.dispatch(Action::Favorite(a.clone()), &ctx);
        app.dispatch(Action::Navigate(View::Library), &ctx);
        assert_eq!(app.library.len(), 2);
        app.filter = b.id.clone();
        app.dispatch(Action::RefreshLibrary, &ctx);
        assert_eq!(app.library, vec![b]);
        app.dispatch(Action::Navigate(View::Favorites), &ctx);
        assert_eq!(app.library, vec![a.clone()]);
        app.dispatch(Action::Favorite(a), &ctx);
        assert!(app.favorites.is_empty());
    }
    #[test]
    fn mute_restores_volume_and_seek_requires_real_capability() {
        let (mut app, commands, _, ctx) = fixture();
        app.volume = 42;
        app.unmuted_volume = 42;
        app.dispatch(Action::Mute, &ctx);
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::SetVolume(0)
        ));
        app.dispatch(Action::Mute, &ctx);
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::SetVolume(42)
        ));
        app.dispatch(Action::Seek(15.), &ctx);
        assert!(commands.try_recv().is_err());
        app.seekable = true;
        app.dispatch(Action::Seek(15.), &ctx);
        assert!(matches!(commands.recv().unwrap(), PlayerCommand::Seek(15.)));
    }
    #[test]
    fn keyboard_search_does_not_lose_fast_input_or_pause_while_typing() {
        let (mut app, commands, _, ctx) = fixture();
        let frame = |app: &mut MusicApp, events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280., 800.),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    app.shortcuts(ui.ctx());
                    crate::ui::show(app, ui);
                },
            );
            output.textures_delta.clear();
        };
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        frame(&mut app, Vec::new());
        frame(
            &mut app,
            vec![
                key(egui::Key::K, egui::Modifiers::COMMAND),
                egui::Event::Text("Tycho Awake".into()),
            ],
        );
        assert_eq!(app.query, "Tycho Awake");
        assert_eq!(app.view, View::Search);
        app.current = Some(track("dQw4w9WgXcQ"));
        app.playback = Playback::Paused;
        frame(
            &mut app,
            vec![
                key(egui::Key::Space, egui::Modifiers::NONE),
                egui::Event::Text(" ".into()),
            ],
        );
        assert!(commands.try_recv().is_err());
        frame(&mut app, vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
        assert!(app.searching);
        assert_eq!(app.submitted_query, "Tycho Awake");
        frame(
            &mut app,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        frame(&mut app, vec![key(egui::Key::Space, egui::Modifiers::NONE)]);
        assert!(matches!(
            commands.recv_timeout(Duration::from_millis(100)).unwrap(),
            PlayerCommand::SetPaused(false)
        ));
    }
}

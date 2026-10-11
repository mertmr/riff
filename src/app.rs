//! Application state and effects. Rendering emits actions; workers never touch UI state.
use crate::{
    account::{self, Request as AccountRequest, Response as AccountResponse},
    catalog::{self, Page, SearchFilter},
    model::{Collection, CollectionKind, Playlist, SavedAlbum, Track, move_item},
    player::{self, PlaybackEvent, PlayerCommand, PlayerEvent},
    radio::Radio,
    search,
    storage::{self, LibraryWriter, Session},
    ui,
};
use std::{
    cell::RefCell,
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
    Albums,
    Artists,
    Playlists,
    Collection,
    Account,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    Saved,
    Favorites,
    Recent,
}

pub enum Action {
    ConnectAccount,
    RefreshAccount,
    DisconnectAccount,
    AccountTab(account::Tab),
    AccountPlaylist(Collection),
    AccountMore,
    AccountBack,
    AccountSearch(String),
    Navigate(View),
    Search,
    CancelSearch,
    FocusSearch,
    Select(usize),
    Play(Track),
    PlayCollectionTrack(usize),
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
    SetCacheAudio(bool),
    PlayFavorites,
    RefreshLibrary,
    LibraryTab(LibraryTab),
    SearchFilter(SearchFilter),
    SearchMore,
    OpenCollection(Collection),
    CollectionMore,
    RetryCollection,
    Back,
    SaveCollection,
    SaveTrack(Track),
    AddToPlaylist(Track),
    OpenPlaylist(u64),
    CreatePlaylist(String, Vec<Track>),
    RenamePlaylist(u64, String),
    DeletePlaylist(u64),
    PlaylistAdd(u64, Track, usize),
    PlaylistRemove(u64, usize),
    PlaylistMove(u64, usize, usize),
    PlaylistDrop(u64, TrackDrag, usize),
    PlayTracks(Vec<Track>),
    QueueTracks(Vec<Track>),
    QueueDrop(TrackDrag, usize),
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

enum CatalogRequest {
    Search(String, SearchFilter, Option<String>),
    Browse(Collection, Option<String>),
}
struct CatalogJob {
    token: u64,
    request: CatalogRequest,
    ctx: egui::Context,
}
struct CatalogResult {
    token: u64,
    detail: bool,
    append: bool,
    result: Result<Page, String>,
}

#[derive(Clone, Debug)]
pub enum DragOrigin {
    Track,
    Queue {
        index: usize,
        revision: u64,
    },
    Playlist {
        id: u64,
        index: usize,
        revision: u64,
    },
}
#[derive(Clone, Debug)]
pub struct TrackDrag {
    pub track: Track,
    pub origin: DragOrigin,
}

pub enum PlaylistEditor {
    Create { title: String, tracks: Vec<Track> },
    Rename { id: u64, title: String },
    Delete { id: u64, title: String },
    Add(Track),
}
struct BackState {
    view: View,
    collection: Option<Collection>,
    page: Page,
    playlist: Option<u64>,
}

#[derive(Clone)]
struct CollectionSnapshot {
    tracks: Arc<Vec<Track>>,
    albums: Arc<Vec<SavedAlbum>>,
    artists: Arc<Vec<Collection>>,
    playlists: Arc<Vec<Playlist>>,
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
    pub account: account::State,
    pub query: String,
    pub submitted_query: String,
    pub results: Vec<Track>,
    pub favorites: Vec<Track>,
    pub recent: Vec<Track>,
    pub library: Vec<Track>,
    pub saved_tracks: Vec<Track>,
    pub albums: Vec<SavedAlbum>,
    pub artists: Vec<Collection>,
    pub playlists: Vec<Playlist>,
    pub playlist_editor: Option<PlaylistEditor>,
    pub playlist_id: Option<u64>,
    pub collection: Option<Collection>,
    pub detail: Page,
    pub detail_loading: bool,
    pub detail_error: Option<String>,
    back_stack: Vec<BackState>,
    pub search_filter: SearchFilter,
    pub search_collections: Vec<Collection>,
    pub search_continuation: Option<String>,
    pub queue_revision: u64,
    pub playlist_revision: u64,
    collection_snapshot: RefCell<Option<CollectionSnapshot>>,
    pub filter: String,
    pub library_tab: LibraryTab,
    pub queue: VecDeque<Track>,
    pub radio: Option<Radio>,
    auto_radio: bool,
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
    pub playback_paused: bool,
    pub position: f64,
    pub duration: Option<f64>,
    pub seekable: bool,
    pub volume: u8,
    unmuted_volume: u8,
    pub cache_audio: bool,
    pub playback_error: Option<String>,
    pub global_error: Option<String>,
    pub notice: Option<Notice>,
    pub focus_search: bool,
    pub artwork: ui::widgets::Artwork,
    search_token: u64,
    search_epoch: Arc<AtomicU64>,
    search_tx: Sender<CatalogJob>,
    search_rx: Receiver<CatalogResult>,
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
    scheduled_next: Option<(u64, u64, Track)>,
    prepared_following: Vec<Track>,
    player_tx: Sender<PlayerCommand>,
    player_rx: Receiver<PlayerEvent>,
    player_worker: Option<thread::JoinHandle<()>>,
    writer: LibraryWriter,
    checkpoint: Instant,
    #[cfg(feature = "ui-qa")]
    pub qa: Option<crate::qa::Capture>,
}
impl MusicApp {
    #[cfg(not(target_os = "macos"))]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::from_context(&cc.egui_ctx)
    }
    pub(crate) fn from_context(ctx: &egui::Context) -> Self {
        let (favorites, session, global_error) = storage::load_library();
        let mut app = Self::from_library(ctx, favorites, session, global_error);
        #[cfg(feature = "ui-qa")]
        let restore = app.qa.is_none();
        #[cfg(not(feature = "ui-qa"))]
        let restore = true;
        if restore {
            app.request_account(AccountRequest::Restore);
        }
        app
    }
    fn from_library(
        ctx: &egui::Context,
        favorites: Vec<Track>,
        session: Session,
        global_error: Option<String>,
    ) -> Self {
        ui::theme::install(ctx);
        let (search_tx, jobs) = mpsc::channel::<CatalogJob>();
        let (result_tx, search_rx) = mpsc::channel();
        let search_epoch = Arc::new(AtomicU64::new(0));
        let cancellation = search_epoch.clone();
        thread::spawn(move || {
            let client = catalog::Client::new();
            while let Ok(mut job) = jobs.recv() {
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                if cancellation.load(Ordering::Relaxed) != job.token {
                    continue;
                }
                let (detail, append, result) = match &job.request {
                    CatalogRequest::Search(query, filter, continuation) => {
                        let result = client.search(query, *filter, continuation.as_deref());
                        // Keep song search available if the music catalog is temporarily unavailable.
                        let result = if result.is_err()
                            && continuation.is_none()
                            && matches!(filter, SearchFilter::All | SearchFilter::Songs)
                        {
                            search::search_youtube(query, || {
                                cancellation.load(Ordering::Relaxed) != job.token
                            })
                            .map(Page::from)
                        } else {
                            result
                        };
                        (false, continuation.is_some(), result)
                    }
                    CatalogRequest::Browse(collection, continuation) => (
                        true,
                        continuation.is_some(),
                        client.browse(collection, continuation.as_deref()),
                    ),
                };
                let _ = result_tx.send(CatalogResult {
                    token: job.token,
                    detail,
                    append,
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
            let client = catalog::Client::new();
            while let Ok(mut job) = jobs.recv() {
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                if cancellation.load(Ordering::Relaxed) != job.token {
                    continue;
                }
                let result = client.recommendations(&job.query).or_else(|error| {
                    if cancellation.load(Ordering::Relaxed) != job.token {
                        return Err(error);
                    }
                    search::radio_youtube(&job.query, || {
                        cancellation.load(Ordering::Relaxed) != job.token
                    })
                });
                if cancellation.load(Ordering::Relaxed) != job.token {
                    continue;
                }
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
            let client = catalog::Client::new();
            while let Ok(mut job) = jobs.recv() {
                while let Ok(newer) = jobs.try_recv() {
                    job = newer;
                }
                if cancellation.load(Ordering::Relaxed) != job.token {
                    continue;
                }
                let result = client.recommendations(&job.query);
                if cancellation.load(Ordering::Relaxed) != job.token {
                    continue;
                }
                let _ = result_tx.send(SearchResult {
                    token: job.token,
                    result,
                });
                job.ctx.request_repaint();
            }
        });
        let mut app = Self {
            account: account::State::new(ctx.clone()),
            query: String::new(),
            submitted_query: String::new(),
            results: Vec::new(),
            favorites,
            recent: session.recent,
            library: Vec::new(),
            saved_tracks: Arc::unwrap_or_clone(session.saved_tracks),
            albums: Arc::unwrap_or_clone(session.albums),
            artists: Arc::unwrap_or_clone(session.artists),
            playlists: Arc::unwrap_or_clone(session.playlists),
            collection_snapshot: RefCell::new(None),
            playlist_editor: None,
            playlist_id: None,
            collection: None,
            detail: Page::default(),
            detail_loading: false,
            detail_error: None,
            back_stack: Vec::new(),
            search_filter: SearchFilter::All,
            search_collections: Vec::new(),
            search_continuation: None,
            queue_revision: 0,
            playlist_revision: 0,
            filter: String::new(),
            library_tab: LibraryTab::All,
            queue: session.queue.into(),
            radio: None,
            auto_radio: true,
            recommendations: Recommendations::default(),
            current: session.current.clone(),
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
            playback: if session.current.is_some() {
                Playback::Stopped
            } else {
                Playback::Empty
            },
            buffering: false,
            playback_paused: false,
            position: session.position,
            duration: None,
            seekable: false,
            volume: session.volume,
            unmuted_volume: session.volume.max(1),
            cache_audio: session.cache_audio,
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
            scheduled_next: None,
            prepared_following: Vec::new(),
            player_tx,
            player_rx,
            player_worker: Some(worker),
            writer: LibraryWriter::new(ctx.clone()),
            checkpoint: Instant::now(),
            #[cfg(feature = "ui-qa")]
            qa: None,
        };
        app.rebuild_library();
        let _ = app.player_tx.send(PlayerCommand::SetVolume(app.volume));
        let _ = app
            .player_tx
            .send(PlayerCommand::SetCacheEnabled(app.cache_audio));
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
            View::Collection => self
                .playlist_id
                .and_then(|id| self.playlists.iter().find(|p| p.id == id))
                .map_or(&self.detail.tracks, |p| &p.tracks),
            View::Albums | View::Artists | View::Playlists => &[],
            View::Account => self
                .account
                .playlist
                .as_ref()
                .map(|(_, p)| p.tracks.as_slice())
                .or_else(|| {
                    (self.account.tab == account::Tab::Likes)
                        .then(|| self.account.library.as_ref().map(|l| l.likes.as_slice()))
                        .flatten()
                })
                .unwrap_or_default(),
            View::Home if !self.recommendations.tracks.is_empty() => &self.recommendations.tracks,
            View::Home => &self.recent[..self.recent.len().min(12)],
        }
    }
    fn collection_snapshot(&self) -> CollectionSnapshot {
        // Volume and queue changes reuse immutable collection snapshots.
        // Large playlists are copied only when the collections themselves change.
        let mut cached = self.collection_snapshot.borrow_mut();
        cached
            .get_or_insert_with(|| CollectionSnapshot {
                tracks: Arc::new(self.saved_tracks.clone()),
                albums: Arc::new(self.albums.clone()),
                artists: Arc::new(self.artists.clone()),
                playlists: Arc::new(self.playlists.clone()),
            })
            .clone()
    }
    fn persist(&self) {
        if !self.write_library {
            return;
        }
        let snapshot = self.collection_snapshot();
        self.writer.save(
            &self.favorites,
            Session {
                recent: self.recent.clone(),
                queue: self.queue.iter().cloned().collect(),
                volume: self.volume,
                cache_audio: self.cache_audio,
                current: self.current.clone(),
                position: self.position,
                saved_tracks: snapshot.tracks.clone(),
                albums: snapshot.albums.clone(),
                artists: snapshot.artists.clone(),
                playlists: snapshot.playlists.clone(),
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
            let mut ids: HashSet<String> = self.library.iter().map(|t| t.id.clone()).collect();
            let tracks = self
                .recent
                .iter()
                .filter(|_| self.library_tab != LibraryTab::Saved)
                .chain(
                    self.saved_tracks
                        .iter()
                        .filter(|_| self.library_tab != LibraryTab::Recent),
                )
                .chain(
                    self.account
                        .library
                        .iter()
                        .flat_map(|l| &l.likes)
                        .filter(|_| self.library_tab != LibraryTab::Recent),
                )
                .chain(
                    self.albums
                        .iter()
                        .filter(|_| self.library_tab != LibraryTab::Recent)
                        .flat_map(|a| &a.tracks),
                )
                .chain(
                    self.playlists
                        .iter()
                        .filter(|_| self.library_tab != LibraryTab::Recent)
                        .flat_map(|p| &p.tracks),
                );
            for track in tracks {
                if ids.insert(track.id.clone()) {
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
    fn request_catalog(&mut self, request: CatalogRequest, ctx: &egui::Context) {
        let detail = matches!(request, CatalogRequest::Browse(..));
        self.search_token = self.search_token.wrapping_add(1);
        self.search_epoch
            .store(self.search_token, Ordering::Relaxed);
        self.searching = !detail;
        self.detail_loading = detail;
        if detail {
            self.detail_error = None;
        } else {
            self.search_error = None;
            self.search_done = false;
        }
        if self
            .search_tx
            .send(CatalogJob {
                token: self.search_token,
                request,
                ctx: ctx.clone(),
            })
            .is_err()
        {
            self.searching = false;
            self.detail_loading = false;
            self.global_error =
                Some("The catalog worker stopped. Restart Riff to reconnect.".into());
        }
    }
    fn cancel_catalog(&mut self) {
        self.search_token = self.search_token.wrapping_add(1);
        self.search_epoch
            .store(self.search_token, Ordering::Relaxed);
        self.searching = false;
        self.detail_loading = false;
    }
    fn push_back_state(&mut self) {
        self.back_stack.push(BackState {
            view: self.view,
            collection: self.collection.clone(),
            page: std::mem::take(&mut self.detail),
            playlist: self.playlist_id,
        });
        if self.back_stack.len() > 16 {
            self.back_stack.remove(0);
        }
    }
    pub fn collection_saved(&self, collection: &Collection) -> bool {
        match collection.kind {
            CollectionKind::Album => self.albums.iter().any(|a| a.collection.id == collection.id),
            CollectionKind::Artist => self.artists.iter().any(|a| a.id == collection.id),
            CollectionKind::Playlist | CollectionKind::Discography => false,
        }
    }
    fn create_playlist(&mut self, title: String, tracks: Vec<Track>) {
        let title = title.trim();
        if title.is_empty() {
            return;
        }
        let Some(id) = self
            .playlists
            .iter()
            .map(|p| p.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
        else {
            return;
        };
        self.playlists.push(Playlist {
            id,
            title: title.chars().take(200).collect(),
            tracks,
        });
        self.collection_snapshot.get_mut().take();
        self.playlist_revision = self.playlist_revision.wrapping_add(1);
        self.rebuild_library();
        self.persist();
        self.notify("Playlist created");
    }
    fn queue_drop(&mut self, drag: TrackDrag, slot: usize) {
        if slot > self.queue_len() {
            return;
        }
        let target = match drag.origin {
            DragOrigin::Queue { index, revision } => {
                if revision != self.queue_revision || self.queue_track(index) != Some(&drag.track) {
                    return;
                }
                let target = slot.saturating_sub(usize::from(index < slot));
                if index == target {
                    return;
                }
                let manual = self.queue.len();
                if index < manual && target < manual {
                    move_item(&mut self.queue, index, slot);
                } else if index >= manual && target >= manual {
                    if let Some(radio) = &mut self.radio {
                        move_item(&mut radio.upcoming, index - manual, slot - manual);
                    }
                } else {
                    // Crossing the priority boundary makes the resulting order explicit.
                    if let Some(radio) = &mut self.radio {
                        self.queue.append(&mut radio.upcoming);
                    }
                    move_item(&mut self.queue, index, slot);
                }
                target
            }
            _ => {
                // Preserve exactly the visible insertion position, including radio suggestions.
                if let Some(radio) = &mut self.radio {
                    while self.queue.len() < slot {
                        let Some(track) = radio.upcoming.pop_front() else {
                            break;
                        };
                        self.queue.push_back(track);
                    }
                    radio.remember(&drag.track.id);
                }
                self.queue.insert(slot, drag.track);
                slot
            }
        };
        self.queue_revision = self.queue_revision.wrapping_add(1);
        self.queue_selected = Some(target);
        self.persist();
    }
    fn play(&mut self, track: Track, add_history: bool) {
        self.play_from(track, add_history, 0.);
    }
    fn play_from(&mut self, track: Track, add_history: bool, position: f64) {
        let selected_at = Instant::now();
        self.begin_track(track.clone(), add_history);
        self.position = if position.is_finite() {
            position.max(0.)
        } else {
            0.
        };
        if self
            .player_tx
            .send(PlayerCommand::Play {
                track,
                generation: self.generation,
                position: self.position,
                paused: false,
                selected_at,
            })
            .is_err()
        {
            self.playback = Playback::Error;
            self.playback_error =
                Some("The audio worker stopped. Restart Riff to reconnect.".into());
        }
        self.checkpoint = Instant::now();
        self.persist();
    }
    fn begin_track(&mut self, track: Track, add_history: bool) {
        self.scheduled_next = None;
        self.prepared_following.clear();
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
        self.current = Some(track);
        self.playback = Playback::Loading;
        self.playback_paused = false;
        self.buffering = false;
        self.position = 0.;
        self.duration = None;
        self.seekable = false;
        self.playback_error = None;
    }
    fn sync_next(&mut self) {
        let head = if matches!(
            self.playback,
            Playback::Loading | Playback::Playing | Playback::Paused
        ) {
            self.queue_track(0)
        } else {
            None
        };
        let same_head =
            head.zip(self.scheduled_next.as_ref())
                .is_some_and(|(track, (generation, _, old))| {
                    *generation == self.generation && track == old
                });
        let same_empty = head.is_none() && self.scheduled_next.is_none();
        let same_following = if head.is_some() {
            (1..4)
                .filter_map(|i| self.queue_track(i))
                .eq(self.prepared_following.iter())
        } else {
            self.prepared_following.is_empty()
        };
        // The unchanged per-frame path compares references without allocating.
        if (same_head || same_empty) && same_following {
            return;
        }
        let desired = head.map(|track| (self.generation, self.queue_revision, track.clone()));
        let following = if desired.is_some() {
            (1..4)
                .filter_map(|i| self.queue_track(i).cloned())
                .collect()
        } else {
            Vec::new()
        };
        // Preserve the identity/revision of the scheduled head when only its
        // following tracks change. A handoff still consumes that same occurrence.
        if !same_head {
            self.scheduled_next = desired;
        }
        self.prepared_following = following;
        let _ = self.player_tx.send(PlayerCommand::PrepareNext {
            track: self
                .scheduled_next
                .as_ref()
                .map(|(_, _, track)| track.clone()),
            generation: self.generation,
            revision: self
                .scheduled_next
                .as_ref()
                .map_or(self.queue_revision, |(_, revision, _)| *revision),
            following: self.prepared_following.clone(),
        });
    }
    fn transition(&mut self, track: Track, revision: u64) {
        if self.scheduled_next.as_ref() != Some(&(self.generation, revision, track.clone()))
            || self.queue_track(0) != Some(&track)
        {
            // A queue edit crossed the native handoff. Replace stale audio with
            // the current queue choice without consuming the removed occurrence.
            // Skip the native entry's generation so its cached metadata cannot
            // be mistaken for the replacement track's playback events.
            self.generation = self.generation.wrapping_add(1);
            self.next();
            return;
        }
        if self.queue.pop_front().is_none()
            && let Some(radio) = &mut self.radio
        {
            radio.upcoming.pop_front();
            radio.continuation = track.clone();
        }
        self.queue_revision = self.queue_revision.wrapping_add(1);
        self.begin_track(track, true);
        self.queue_selected = None;
        self.persist();
        self.refill_radio();
    }
    fn next(&mut self) {
        self.queue_revision = self.queue_revision.wrapping_add(1);
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
            if self.auto_radio
                && self.radio.is_none()
                && let Some(seed) = self.current.clone()
            {
                let mut radio = Radio::new(seed);
                for track in self.history.iter().chain(&self.recent) {
                    radio.remember(&track.id);
                }
                self.radio = Some(radio);
            }
            self.generation = self.generation.wrapping_add(1);
            let _ = self.player_tx.send(PlayerCommand::Stop);
            self.playback = Playback::Stopped;
            self.buffering = false;
            self.position = 0.;
            self.persist();
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
        self.recent
            .first()
            .or_else(|| {
                self.account
                    .library
                    .as_ref()
                    .and_then(account::Library::seed)
            })
            .or(self.favorites.first())
            .cloned()
    }
    fn refresh_recommendations(&mut self, force: bool) {
        #[cfg(feature = "ui-qa")]
        if self.qa.is_some() {
            return;
        }
        let seed = self.recommendation_seed();
        let same_seed =
            self.recommendations.seed.as_ref().map(|t| &t.id) == seed.as_ref().map(|t| &t.id);
        if same_seed && (!force || self.recommendations.searching) {
            return;
        }
        self.recommendation_token = self.recommendation_token.wrapping_add(1);
        self.recommendation_epoch
            .store(self.recommendation_token, Ordering::Relaxed);
        let Some(seed) = seed else {
            self.recommendations = Recommendations::default();
            self.selected = None;
            return;
        };
        self.recommendations.seed = Some(seed.clone());
        self.recommendations.error = None;
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
    /// Playback duration when mpv has reported one, otherwise the catalog's
    /// duration estimate so seeking never waits on demuxer metadata.
    pub fn effective_duration(&self) -> Option<f64> {
        self.duration.or(self
            .current
            .as_ref()
            .and_then(|t| t.duration_seconds.map(|s| s as f64)))
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
        index < self.queue_len() && target < self.queue_len()
    }
    pub(crate) fn receive(&mut self) {
        while let Ok(message) = self.account.worker.rx.try_recv() {
            if message.token != self.account.worker.token {
                continue;
            }
            self.account.loading = false;
            match message.result {
                Ok(AccountResponse::Library(library)) => {
                    self.account.library = library;
                    self.account.error = None;
                    self.rebuild_library();
                    self.refresh_recommendations(true);
                }
                Ok(AccountResponse::Playlist { id, append, page }) => {
                    if let Some((collection, current)) = &mut self.account.playlist
                        && collection.id == id
                    {
                        if append {
                            current.append_collection(page);
                        } else {
                            *current = page;
                        }
                    }
                }
                Err(error) => self.account.error = Some(error),
            }
        }
        while let Ok(message) = self.recommendation_rx.try_recv() {
            if message.token != self.recommendation_token || self.recommendations.seed.is_none() {
                continue;
            }
            let known: Vec<Track> = self
                .recent
                .iter()
                .chain(&self.favorites)
                .chain(self.account.library.iter().flat_map(|l| &l.likes))
                .chain(&self.queue)
                .chain(self.current.iter())
                .cloned()
                .collect();
            let refreshed = message.result.is_ok();
            self.recommendations.complete(message.result, &known);
            if refreshed {
                self.selected = None;
            }
        }
        while let Ok(message) = self.radio_rx.try_recv() {
            self.queue_revision = self.queue_revision.wrapping_add(1);
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
            if message.detail {
                self.detail_loading = false;
                match message.result {
                    Ok(page) => {
                        if message.append {
                            self.detail.append_collection(page);
                        } else {
                            self.detail = page;
                        }
                        self.detail_error = None;
                    }
                    Err(error) => self.detail_error = Some(error),
                }
            } else {
                self.searching = false;
                self.search_done = true;
                match message.result {
                    Ok(page) => {
                        if message.append {
                            let mut current = Page {
                                tracks: std::mem::take(&mut self.results),
                                collections: std::mem::take(&mut self.search_collections),
                                continuation: None,
                            };
                            current.append(page);
                            self.results = current.tracks;
                            self.search_collections = current.collections;
                            self.search_continuation = current.continuation;
                        } else {
                            self.results = page.tracks;
                            self.search_collections = page.collections;
                            self.search_continuation = page.continuation;
                            self.selected = None;
                        }
                        self.search_error = None;
                    }
                    Err(error) => self.search_error = Some(error),
                }
            }
        }
        while let Ok(message) = self.player_rx.try_recv() {
            if message.generation != self.generation {
                continue;
            }
            match message.event {
                PlaybackEvent::Transition { track, revision } => self.transition(track, revision),
                PlaybackEvent::Recovering => {
                    self.playback = Playback::Loading;
                    self.buffering = true;
                    self.seekable = false;
                    self.playback_error = None;
                    self.persist();
                }
                PlaybackEvent::Ready => {
                    self.playback = if self.playback_paused {
                        Playback::Paused
                    } else {
                        Playback::Playing
                    };
                    self.buffering = false;
                    if let Some(track) = &self.current {
                        self.recent.retain(|t| t.id != track.id);
                        self.recent.insert(0, track.clone());
                        self.recent.truncate(40);
                        self.rebuild_library();
                        self.persist();
                    }
                }
                PlaybackEvent::Paused(paused) => {
                    self.playback_paused = paused;
                    if matches!(self.playback, Playback::Playing | Playback::Paused) {
                        self.playback = if paused {
                            Playback::Paused
                        } else {
                            Playback::Playing
                        };
                        self.persist();
                    }
                }
                PlaybackEvent::Buffering(value) => self.buffering = value,
                PlaybackEvent::Position(value) => {
                    self.position = value;
                    if self.checkpoint.elapsed() >= Duration::from_secs(5) {
                        self.checkpoint = Instant::now();
                        self.persist();
                    }
                }
                PlaybackEvent::Duration(value) => self.duration = Some(value),
                PlaybackEvent::Seekable(value) => self.seekable = value,
                PlaybackEvent::Stopped => {
                    self.playback = Playback::Stopped;
                    self.buffering = false;
                    self.persist();
                }
                PlaybackEvent::Ended => self.next(),
                PlaybackEvent::Error(e) => {
                    self.playback = Playback::Error;
                    self.playback_error = Some(e);
                    self.buffering = false;
                    self.seekable = false;
                    self.persist();
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
        self.sync_next();
    }
    fn request_account(&mut self, request: AccountRequest) {
        self.account.error = None;
        self.account.loading = true;
        if let Err(error) = self.account.worker.request(request) {
            self.account.error = Some(error);
            self.account.loading = false;
        }
    }
    pub fn dispatch(&mut self, action: Action, ctx: &egui::Context) {
        if matches!(
            &action,
            Action::SaveTrack(..)
                | Action::SaveCollection
                | Action::RenamePlaylist(..)
                | Action::DeletePlaylist(..)
                | Action::PlaylistAdd(..)
                | Action::PlaylistRemove(..)
                | Action::PlaylistMove(..)
        ) {
            self.collection_snapshot.get_mut().take();
        }
        if matches!(
            &action,
            Action::Enqueue(..)
                | Action::QueuePlay(..)
                | Action::QueueRemove(..)
                | Action::QueueMove(..)
                | Action::ClearQueue
                | Action::PlayFavorites
                | Action::Previous
                | Action::StartRadio(..)
                | Action::StopRadio
        ) {
            self.queue_revision = self.queue_revision.wrapping_add(1);
        }
        match action {
            Action::ConnectAccount => {
                let config = if self.account.credentials_json.trim().is_empty() {
                    Ok(self.account.config.clone())
                } else {
                    account::Config::from_json(&self.account.credentials_json)
                };
                match config {
                    Ok(config) => {
                        self.account.config = config.clone();
                        self.account.credentials_json.clear();
                        self.request_account(AccountRequest::Connect(config));
                    }
                    Err(error) => self.account.error = Some(error),
                }
            }
            Action::RefreshAccount => self.request_account(AccountRequest::Refresh),
            Action::DisconnectAccount => {
                self.account.library = None;
                self.account.playlist = None;
                self.rebuild_library();
                self.refresh_recommendations(true);
                self.request_account(AccountRequest::Disconnect);
            }
            Action::AccountTab(tab) => {
                self.account.tab = tab;
                self.account.playlist = None;
                self.selected = None;
            }
            Action::AccountBack => {
                self.account.playlist = None;
                self.selected = None;
            }
            Action::AccountSearch(query) => {
                self.query = query;
                self.dispatch(Action::Search, ctx);
            }
            Action::AccountPlaylist(collection) => {
                let id = collection.id.clone();
                self.account.playlist = Some((collection, Page::default()));
                self.selected = None;
                self.request_account(AccountRequest::Playlist(id, None));
            }
            Action::AccountMore => {
                if let Some((collection, page)) = &self.account.playlist {
                    self.request_account(AccountRequest::Playlist(
                        collection.id.clone(),
                        page.continuation.clone(),
                    ));
                }
            }
            Action::Navigate(view) => {
                self.cancel_catalog();
                self.back_stack.clear();
                self.collection = None;
                self.playlist_id = None;
                self.detail = Page::default();
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
                self.cancel_catalog();
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
                self.search_collections.clear();
                self.search_continuation = None;
                self.results.clear();
                self.selected = None;
                self.request_catalog(
                    CatalogRequest::Search(self.submitted_query.clone(), self.search_filter, None),
                    ctx,
                );
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
                self.auto_radio = true;
                self.play(track, true);
            }
            Action::PlayCollectionTrack(index) => {
                if self.view == View::Collection
                    && let Some(tracks) = self.active_tracks().get(index..)
                    && !tracks.is_empty()
                {
                    self.dispatch(Action::PlayTracks(tracks.to_vec()), ctx);
                }
            }
            Action::StartRadio(track) => {
                self.stop_radio();
                self.auto_radio = true;
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
                self.auto_radio = false;
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
                Playback::Loading | Playback::Playing | Playback::Paused => {
                    self.playback_paused = if self.playback == Playback::Loading {
                        !self.playback_paused
                    } else {
                        self.playback != Playback::Paused
                    };
                    if self.playback != Playback::Loading {
                        self.playback = if self.playback_paused {
                            Playback::Paused
                        } else {
                            Playback::Playing
                        };
                    }
                    let _ = self
                        .player_tx
                        .send(PlayerCommand::SetPaused(self.playback_paused));
                }
                Playback::Stopped | Playback::Error => {
                    if let Some(t) = self.current.clone() {
                        self.auto_radio = true;
                        self.play_from(t, false, self.position);
                    }
                }
                _ => {}
            },
            Action::Previous => {
                if self.position > 3. {
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
                self.auto_radio = false;
                self.generation = self.generation.wrapping_add(1);
                let _ = self.player_tx.send(PlayerCommand::Stop);
                self.playback = Playback::Stopped;
                self.buffering = false;
                self.position = 0.;
                self.persist();
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
                    self.auto_radio = true;
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
                    self.queue_drop(
                        TrackDrag {
                            track: self.queue_track(i).unwrap().clone(),
                            origin: DragOrigin::Queue {
                                index: i,
                                revision: self.queue_revision,
                            },
                        },
                        target + usize::from(i < target),
                    );
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
                if self.volume == v {
                    return;
                }
                self.volume = v;
                if v > 0 {
                    self.unmuted_volume = v;
                }
                let _ = self.player_tx.send(PlayerCommand::SetVolume(v));
                ctx.request_repaint();
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
            Action::SetCacheAudio(enabled) => {
                if self.cache_audio == enabled {
                    return;
                }
                self.cache_audio = enabled;
                let _ = self.player_tx.send(PlayerCommand::SetCacheEnabled(enabled));
                ctx.request_repaint();
                self.persist();
            }
            Action::Seek(p) => {
                if self.current.is_some() && p.is_finite() {
                    let max = self.effective_duration().unwrap_or(f64::MAX);
                    self.position = p.clamp(0., max);
                    if matches!(
                        self.playback,
                        Playback::Loading | Playback::Playing | Playback::Paused
                    ) {
                        let _ = self.player_tx.send(PlayerCommand::Seek(self.position));
                    }
                    self.persist();
                }
            }
            Action::PlayFavorites => {
                self.stop_radio();
                if let Some(first) = self.favorites.first().cloned() {
                    self.auto_radio = true;
                    self.queue = self.favorites.iter().skip(1).cloned().collect();
                    self.play(first, true);
                    self.persist();
                }
            }
            Action::RefreshLibrary => self.rebuild_library(),
            Action::SearchFilter(filter) => {
                self.search_filter = filter;
                if !self.query.trim().is_empty() {
                    self.dispatch(Action::Search, ctx);
                }
            }
            Action::SearchMore => {
                if !self.searching
                    && let Some(token) = self.search_continuation.clone()
                {
                    self.request_catalog(
                        CatalogRequest::Search(
                            self.submitted_query.clone(),
                            self.search_filter,
                            Some(token),
                        ),
                        ctx,
                    );
                }
            }
            Action::OpenCollection(collection) => {
                self.cancel_catalog();
                self.push_back_state();
                self.view = View::Collection;
                self.playlist_id = None;
                self.selected = None;
                self.filter.clear();
                self.collection = Some(collection.clone());
                if let Some(album) = self
                    .albums
                    .iter()
                    .find(|a| a.collection.id == collection.id)
                {
                    self.detail.tracks = album.tracks.clone();
                }
                self.request_catalog(CatalogRequest::Browse(collection, None), ctx);
            }
            Action::CollectionMore => {
                if !self.detail_loading
                    && let (Some(collection), Some(token)) =
                        (self.collection.clone(), self.detail.continuation.clone())
                {
                    self.request_catalog(CatalogRequest::Browse(collection, Some(token)), ctx);
                }
            }
            Action::RetryCollection => {
                if let Some(collection) = self.collection.clone() {
                    let token =
                        if self.detail.tracks.is_empty() && self.detail.collections.is_empty() {
                            None
                        } else {
                            self.detail.continuation.clone()
                        };
                    self.request_catalog(CatalogRequest::Browse(collection, token), ctx);
                }
            }
            Action::Back => {
                self.cancel_catalog();
                if let Some(state) = self.back_stack.pop() {
                    self.view = state.view;
                    self.collection = state.collection;
                    self.detail = state.page;
                    self.playlist_id = state.playlist;
                    self.detail_error = None;
                    self.selected = None;
                    self.filter.clear();
                } else {
                    self.dispatch(Action::Navigate(View::Library), ctx);
                }
            }
            Action::SaveTrack(track) => {
                if self.saved_tracks.iter().any(|t| t.id == track.id) {
                    self.saved_tracks.retain(|t| t.id != track.id);
                    self.notify("Removed from saved songs");
                } else {
                    self.saved_tracks.push(track);
                    self.notify("Saved to library");
                }
                self.rebuild_library();
                self.persist();
            }
            Action::AddToPlaylist(track) => self.playlist_editor = Some(PlaylistEditor::Add(track)),
            Action::SaveCollection => {
                if let Some(collection) = self.collection.clone() {
                    match collection.kind {
                        CollectionKind::Album => {
                            if self.collection_saved(&collection) {
                                self.albums.retain(|a| a.collection.id != collection.id);
                            } else if !self.detail_loading
                                && self.detail_error.is_none()
                                && self.detail.continuation.is_none()
                            {
                                self.albums.push(SavedAlbum {
                                    collection,
                                    tracks: self.detail.tracks.clone(),
                                });
                            }
                        }
                        CollectionKind::Artist => {
                            if self.collection_saved(&collection) {
                                self.artists.retain(|a| a.id != collection.id);
                            } else {
                                self.artists.push(collection);
                            }
                        }
                        CollectionKind::Playlist => {
                            if !self.detail_loading
                                && self.detail_error.is_none()
                                && self.detail.continuation.is_none()
                            {
                                self.create_playlist(collection.title, self.detail.tracks.clone());
                            }
                        }
                        CollectionKind::Discography => {}
                    }
                    self.rebuild_library();
                    self.persist();
                }
            }
            Action::OpenPlaylist(id) => {
                if self.playlists.iter().any(|p| p.id == id) {
                    self.cancel_catalog();
                    self.push_back_state();
                    self.view = View::Collection;
                    self.playlist_id = Some(id);
                    self.collection = None;
                    self.selected = None;
                    self.filter.clear();
                }
            }
            Action::CreatePlaylist(title, tracks) => self.create_playlist(title, tracks),
            Action::RenamePlaylist(id, title) => {
                if !title.trim().is_empty()
                    && let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id)
                {
                    playlist.title = title.trim().chars().take(200).collect();
                    self.persist();
                }
            }
            Action::DeletePlaylist(id) => {
                self.playlists.retain(|p| p.id != id);
                self.playlist_revision = self.playlist_revision.wrapping_add(1);
                if self.playlist_id == Some(id) {
                    self.dispatch(Action::Navigate(View::Playlists), ctx);
                }
                self.rebuild_library();
                self.persist();
            }
            Action::PlaylistAdd(id, track, slot) => {
                if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id) {
                    playlist
                        .tracks
                        .insert(slot.min(playlist.tracks.len()), track);
                    self.playlist_revision = self.playlist_revision.wrapping_add(1);
                    self.rebuild_library();
                    self.persist();
                    self.notify("Added to playlist");
                }
            }
            Action::PlaylistRemove(id, index) => {
                if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id)
                    && index < playlist.tracks.len()
                {
                    playlist.tracks.remove(index);
                    self.playlist_revision = self.playlist_revision.wrapping_add(1);
                    self.rebuild_library();
                    self.persist();
                }
            }
            Action::PlaylistMove(id, from, slot) => {
                if let Some(playlist) = self.playlists.iter_mut().find(|p| p.id == id)
                    && from < playlist.tracks.len()
                    && slot <= playlist.tracks.len()
                {
                    let to = slot.saturating_sub(usize::from(from < slot));
                    if to > from {
                        playlist.tracks[from..=to].rotate_left(1);
                    } else if to < from {
                        playlist.tracks[to..=from].rotate_right(1);
                    }
                    self.playlist_revision = self.playlist_revision.wrapping_add(1);
                    self.persist();
                }
            }
            Action::PlaylistDrop(id, drag, slot) => {
                if let DragOrigin::Playlist {
                    id: source,
                    index,
                    revision,
                } = drag.origin
                    && source == id
                {
                    if revision == self.playlist_revision
                        && self
                            .playlists
                            .iter()
                            .find(|p| p.id == id)
                            .and_then(|p| p.tracks.get(index))
                            == Some(&drag.track)
                    {
                        self.dispatch(Action::PlaylistMove(id, index, slot), ctx);
                    }
                } else {
                    self.dispatch(Action::PlaylistAdd(id, drag.track, slot), ctx);
                }
            }
            Action::PlayTracks(tracks) => {
                if let Some(first) = tracks.first().cloned() {
                    self.stop_radio();
                    self.auto_radio = true;
                    self.queue = tracks.into_iter().skip(1).collect();
                    self.queue_revision = self.queue_revision.wrapping_add(1);
                    self.queue_selected = None;
                    self.play(first, true);
                    self.persist();
                }
            }
            Action::QueueTracks(tracks) => {
                self.queue.extend(tracks);
                self.queue_revision = self.queue_revision.wrapping_add(1);
                self.persist();
                self.notify("Added collection to queue");
            }
            Action::QueueDrop(drag, slot) => self.queue_drop(drag, slot),
            Action::LibraryTab(tab) => {
                self.library_tab = tab;
                self.rebuild_library();
            }
            Action::DismissError => self.global_error = None,
            Action::DismissNotice => self.notice = None,
        }
        self.sync_next();
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.playlist_editor.is_some() {
            return;
        }
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
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Period)) {
            self.dispatch(Action::Stop, ctx);
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
                let start = self.selected.map_or(0, |i| (i + 1).min(count - 1));
                let needle = self.filter.trim().to_lowercase();
                self.selected = (start..count)
                    .find(|i| {
                        self.view != View::Collection
                            || needle.is_empty()
                            || self.active_tracks()[*i]
                                .title
                                .to_lowercase()
                                .contains(&needle)
                            || self.active_tracks()[*i]
                                .artist
                                .to_lowercase()
                                .contains(&needle)
                    })
                    .or(self.selected);
                self.reveal_selection = true;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                let start = self.selected.unwrap_or(0).saturating_sub(1);
                let needle = self.filter.trim().to_lowercase();
                self.selected = (0..=start)
                    .rev()
                    .find(|i| {
                        self.view != View::Collection
                            || needle.is_empty()
                            || self.active_tracks()[*i]
                                .title
                                .to_lowercase()
                                .contains(&needle)
                            || self.active_tracks()[*i]
                                .artist
                                .to_lowercase()
                                .contains(&needle)
                    })
                    .or(self.selected);
                self.reveal_selection = true;
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                && let Some(i) = self.selected
                && let Some(track) = self.active_tracks().get(i).cloned()
            {
                self.dispatch(
                    if self.view == View::Collection {
                        Action::PlayCollectionTrack(i)
                    } else {
                        Action::Play(track)
                    },
                    ctx,
                );
            }
        }
    }
    pub fn can_previous(&self) -> bool {
        !self.history.is_empty() || self.position > 3.
    }
    pub fn can_next(&self) -> bool {
        self.queue_len() > 0
            || self.radio.is_some()
            || (self.auto_radio
                && self.current.is_some()
                && matches!(
                    self.playback,
                    Playback::Loading | Playback::Playing | Playback::Paused
                ))
    }
}
impl MusicApp {
    pub(crate) fn draw_ui(&mut self, ui: &mut egui::Ui) {
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
        if self.searching
            || self.account.loading
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
#[cfg(not(target_os = "macos"))]
impl eframe::App for MusicApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // eframe also calls logic for covered/minimized windows. Drain events
        // and advance the queue there, without rendering or retaining a backlog.
        self.receive();
        #[cfg(feature = "ui-qa")]
        crate::qa::report_startup(_frame.info().cpu_usage.is_some());
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw_ui(ui);
    }
}
impl Drop for MusicApp {
    fn drop(&mut self) {
        self.persist();
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
            ..Default::default()
        }
    }
    fn fixture() -> (
        MusicApp,
        Receiver<PlayerCommand>,
        Sender<PlayerEvent>,
        egui::Context,
    ) {
        let ctx = egui::Context::default();
        let mut app = MusicApp::from_library(&ctx, Vec::new(), Session::default(), None);
        app.write_library = false;
        app.account.worker = account::Worker::fixture().0;
        app.favorites.clear();
        app.recent.clear();
        app.saved_tracks.clear();
        app.albums.clear();
        app.artists.clear();
        app.playlists.clear();
        app.queue.clear();
        app.recommendations = Recommendations::default();
        app.rebuild_library();
        let (tx, commands) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let (search_tx, search_jobs) = mpsc::channel::<CatalogJob>();
        thread::spawn(move || while search_jobs.recv().is_ok() {});
        app.search_tx = search_tx;
        let (recommendation_tx, recommendation_jobs) = mpsc::channel::<SearchJob>();
        thread::spawn(move || while recommendation_jobs.recv().is_ok() {});
        app.recommendation_tx = recommendation_tx;
        let (radio_tx, radio_jobs) = mpsc::channel::<SearchJob>();
        thread::spawn(move || while radio_jobs.recv().is_ok() {});
        app.radio_tx = radio_tx;
        app.player_tx = tx;
        app.player_rx = rx;
        (app, commands, events, ctx)
    }
    #[test]
    fn account_import_is_isolated_and_disconnect_clears_remote_data_and_stale_results() {
        let (mut app, commands, _, ctx) = fixture();
        let (worker, results) = account::Worker::fixture();
        app.account.worker = worker;
        let remote = track("dQw4w9WgXcQ");
        let local = track("hTWKbfoikeg");
        app.favorites.push(local.clone());
        app.request_account(AccountRequest::Refresh);
        let token = app.account.worker.token;
        results
            .send(account::Message {
                token: token.wrapping_sub(1),
                result: Ok(AccountResponse::Library(Some(account::Library::default()))),
            })
            .unwrap();
        app.receive();
        assert!(app.account.library.is_none());
        assert!(app.account.loading);
        results
            .send(account::Message {
                token,
                result: Ok(AccountResponse::Library(Some(account::Library {
                    likes: vec![remote.clone(), local.clone()],
                    ..Default::default()
                }))),
            })
            .unwrap();
        app.receive();
        assert!(!app.account.loading);
        assert_eq!(app.library, [local.clone(), remote.clone()]);
        assert_eq!(app.favorites, std::slice::from_ref(&local));
        assert_eq!(app.recommendation_seed(), Some(remote.clone()));
        app.view = View::Account;
        assert_eq!(app.active_tracks(), [remote.clone(), local.clone()]);
        app.dispatch(Action::AccountTab(account::Tab::Subscriptions), &ctx);
        assert!(app.active_tracks().is_empty());
        app.dispatch(Action::DisconnectAccount, &ctx);
        assert!(app.account.library.is_none());
        assert!(app.account.playlist.is_none());
        assert_eq!(app.library, std::slice::from_ref(&local));
        assert_eq!(app.recommendation_seed(), Some(local));
        results
            .send(account::Message {
                token,
                result: Ok(AccountResponse::Library(Some(account::Library::default()))),
            })
            .unwrap();
        app.receive();
        assert!(app.account.library.is_none());
        assert!(
            commands
                .try_iter()
                .all(|c| !matches!(c, PlayerCommand::Play { .. }))
        );
    }
    #[test]
    fn account_playlist_results_keep_repeats_and_cannot_replace_another_playlist() {
        let (mut app, _, _, ctx) = fixture();
        let (worker, results) = account::Worker::fixture();
        app.account.worker = worker;
        let collection = Collection {
            id: "private-one".into(),
            title: "Private playlist".into(),
            subtitle: String::new(),
            kind: CollectionKind::Playlist,
            thumbnail: None,
            params: None,
        };
        app.dispatch(Action::AccountPlaylist(collection), &ctx);
        let token = app.account.worker.token;
        let first = track("dQw4w9WgXcQ");
        results
            .send(account::Message {
                token,
                result: Ok(AccountResponse::Playlist {
                    id: "different".into(),
                    append: false,
                    page: Page::from(vec![first.clone()]),
                }),
            })
            .unwrap();
        app.receive();
        assert!(app.account.playlist.as_ref().unwrap().1.tracks.is_empty());
        results
            .send(account::Message {
                token,
                result: Ok(AccountResponse::Playlist {
                    id: "private-one".into(),
                    append: false,
                    page: Page {
                        tracks: vec![first.clone()],
                        continuation: Some("next".into()),
                        ..Default::default()
                    },
                }),
            })
            .unwrap();
        app.receive();
        app.dispatch(Action::AccountMore, &ctx);
        results
            .send(account::Message {
                token: app.account.worker.token,
                result: Ok(AccountResponse::Playlist {
                    id: "private-one".into(),
                    append: true,
                    page: Page::from(vec![first.clone()]),
                }),
            })
            .unwrap();
        app.receive();
        assert_eq!(
            app.account.playlist.as_ref().unwrap().1.tracks,
            [first.clone(), first]
        );
        assert!(
            app.account
                .playlist
                .as_ref()
                .unwrap()
                .1
                .continuation
                .is_none()
        );
    }
    #[test]
    fn saved_session_restores_current_track_and_position_without_autoplay() {
        let ctx = egui::Context::default();
        let current = track("dQw4w9WgXcQ");
        let queued = track("hTWKbfoikeg");
        let saved = Session {
            current: Some(current.clone()),
            position: 42.5,
            volume: 37,
            queue: vec![queued.clone()],
            recent: vec![current.clone()],
            ..Default::default()
        };
        let restored: Session =
            serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut app = MusicApp::from_library(&ctx, Vec::new(), restored, None);
        app.write_library = false;
        assert_eq!(app.current, Some(current));
        assert_eq!(app.position, 42.5);
        assert_eq!(app.volume, 37);
        assert_eq!(app.queue.front(), Some(&queued));
        assert!(app.playback == Playback::Stopped);
        assert!(app.player_rx.try_recv().is_err());
    }
    #[test]
    fn recovery_and_manual_retry_preserve_position_without_advancing_queue() {
        let (mut app, commands, events, ctx) = fixture();
        let current = track("dQw4w9WgXcQ");
        app.dispatch(
            Action::PlayTracks(vec![current.clone(), track("hTWKbfoikeg")]),
            &ctx,
        );
        commands.try_iter().for_each(drop);
        for event in [
            PlaybackEvent::Ready,
            PlaybackEvent::Position(42.5),
            PlaybackEvent::Recovering,
        ] {
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event,
                })
                .unwrap();
        }
        app.receive();
        assert!(app.buffering && app.playback == Playback::Loading);
        assert_eq!(app.position, 42.5);
        assert_eq!(app.queue.len(), 1);
        events
            .send(PlayerEvent {
                generation: app.generation,
                event: PlaybackEvent::Error("offline".into()),
            })
            .unwrap();
        app.receive();
        commands.try_iter().for_each(drop);
        app.dispatch(Action::TogglePlayback, &ctx);
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::Play { track, position: 42.5, .. } if track == current)
        );
        assert_eq!(app.queue.len(), 1);
        commands.try_iter().for_each(drop);
        // A restored session waits for Play and uses the same resume path.
        app.playback = Playback::Stopped;
        app.position = 19.;
        app.dispatch(Action::TogglePlayback, &ctx);
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::Play { position: 19., .. }
        ));
        app.dispatch(Action::Stop, &ctx);
        assert_eq!(app.position, 0.);
        assert!(!app.buffering);
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
        assert_eq!(recommendations.tracks, new[..12]);
        assert_eq!(recommendations.error.as_deref(), Some("offline"));
        recommendations.complete(Ok(vec![]), &[]);
        assert!(recommendations.tracks.is_empty());
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
    fn refreshing_recommendations_keeps_cards_coalesces_clicks_and_survives_failure() {
        let (mut app, commands, _, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.recommendation_tx = tx;
        app.recommendation_rx = rx;
        app.favorites.push(track("dQw4w9WgXcQ"));
        app.dispatch(Action::Navigate(View::Home), &ctx);
        let initial = jobs.recv().unwrap();
        let previous = vec![track("4NRXx6U8ABQ"), track("hTWKbfoikeg")];
        results
            .send(SearchResult {
                token: initial.token,
                result: Ok(previous.clone()),
            })
            .unwrap();
        app.receive();
        app.selected = Some(1);
        app.dispatch(Action::RefreshRecommendations, &ctx);
        let refresh = jobs.recv().unwrap();
        for _ in 0..5 {
            app.dispatch(Action::RefreshRecommendations, &ctx);
        }
        assert!(jobs.try_recv().is_err());
        assert_eq!(app.recommendation_token, refresh.token);
        assert_eq!(app.recommendations.tracks, previous);
        assert_eq!(app.active_tracks(), previous);
        assert_eq!(app.selected, Some(1));
        assert!(app.recommendations.searching);

        ctx.enable_accesskit();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280., 800.),
                )),
                ..Default::default()
            },
            |ui| crate::ui::show(&mut app, ui),
        );
        output.textures_delta.clear();
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.label() == Some("Refreshing…") && node.is_disabled())
        );
        assert!(nodes.iter().any(
            |(_, node)| node.label() == Some("Play 4NRXx6U8ABQ · Artist") && !node.is_disabled()
        ));
        assert!(!format!("{:?}", output.shapes).contains("Finding songs for you"));

        results
            .send(SearchResult {
                token: refresh.token,
                result: Err("offline".into()),
            })
            .unwrap();
        app.receive();
        assert!(!app.recommendations.searching);
        assert_eq!(app.recommendations.tracks, previous);
        assert_eq!(app.selected, Some(1));
        assert_eq!(app.recommendations.error.as_deref(), Some("offline"));
        app.dispatch(Action::RefreshRecommendations, &ctx);
        let retry = jobs.recv().unwrap();
        let next = vec![track("JGwWNGJdvx8")];
        results
            .send(SearchResult {
                token: retry.token,
                result: Ok(next.clone()),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.recommendations.tracks, next);
        assert!(app.selected.is_none());
        assert!(app.recommendations.error.is_none());
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn discovery_shortcuts_submit_search_without_playing_or_editing_queue() {
        let (mut app, commands, _, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        app.search_tx = tx;
        app.dispatch(Action::Discover("chill lofi music"), &ctx);
        assert_eq!(app.view, View::Search);
        assert!(
            matches!(jobs.recv().unwrap().request, CatalogRequest::Search(query, _, _) if query == "chill lofi music")
        );
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
    fn exhausted_queue_starts_radio_from_the_last_track_and_skips_played_songs() {
        let (mut app, commands, events, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.radio_tx = tx;
        app.radio_rx = rx;
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.dispatch(Action::PlayTracks(vec![a.clone(), b.clone()]), &ctx);
        assert!(jobs.try_recv().is_err());
        for _ in 0..2 {
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event: PlaybackEvent::Ended,
                })
                .unwrap();
            app.receive();
        }
        let job = jobs.try_recv().unwrap();
        assert_eq!(job.query, b.id);
        assert!(jobs.try_recv().is_err());
        let radio = app.radio.as_ref().unwrap();
        assert_eq!(radio.seed, b);
        assert!(radio.searching && radio.waiting);
        assert!(!app.queue_open);
        commands.try_iter().for_each(drop);
        results
            .send(SearchResult {
                token: job.token,
                result: Ok(vec![a, b, c.clone(), c.clone()]),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.current, Some(c.clone()));
        assert!(!app.radio.as_ref().unwrap().waiting);
        assert!(
            matches!(commands.try_recv().unwrap(), PlayerCommand::Play { track, .. } if track == c)
        );
        assert_eq!(jobs.try_recv().unwrap().query, c.id);
    }

    #[test]
    fn automatic_radio_respects_stop_and_restarts_after_a_new_selection() {
        for stop in [Action::Stop, Action::StopRadio] {
            let (mut app, commands, events, ctx) = fixture();
            let (tx, jobs) = mpsc::channel();
            let (results, rx) = mpsc::channel();
            app.radio_tx = tx;
            app.radio_rx = rx;
            let a = track("dQw4w9WgXcQ");
            let b = track("hTWKbfoikeg");
            app.dispatch(Action::Play(a), &ctx);
            assert!(app.can_next());
            app.dispatch(Action::Next, &ctx);
            let job = jobs.try_recv().unwrap();
            app.dispatch(stop, &ctx);
            commands.try_iter().for_each(drop);
            results
                .send(SearchResult {
                    token: job.token,
                    result: Ok(vec![b.clone()]),
                })
                .unwrap();
            app.receive();
            assert!(app.radio.is_none());
            assert!(!app.can_next());
            assert!(commands.try_recv().is_err());
            app.dispatch(Action::Next, &ctx);
            assert!(jobs.try_recv().is_err());
            app.dispatch(Action::Play(b.clone()), &ctx);
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event: PlaybackEvent::Ended,
                })
                .unwrap();
            app.receive();
            assert_eq!(jobs.try_recv().unwrap().query, b.id);
        }
    }

    #[test]
    fn automatic_radio_failures_wait_for_retry_and_manual_tracks_keep_priority() {
        let (mut app, commands, _, ctx) = fixture();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        app.radio_tx = tx;
        app.radio_rx = rx;
        app.dispatch(Action::Next, &ctx);
        assert!(jobs.try_recv().is_err());
        let a = track("dQw4w9WgXcQ");
        let manual = track("hTWKbfoikeg");
        let related = track("JGwWNGJdvx8");
        app.dispatch(Action::Play(a.clone()), &ctx);
        app.dispatch(Action::Next, &ctx);
        let job = jobs.try_recv().unwrap();
        results
            .send(SearchResult {
                token: job.token,
                result: Err("offline".into()),
            })
            .unwrap();
        app.receive();
        assert_eq!(
            app.radio.as_ref().unwrap().error.as_deref(),
            Some("offline")
        );
        app.dispatch(Action::Next, &ctx);
        assert!(jobs.try_recv().is_err());
        app.dispatch(Action::RetryRadio, &ctx);
        let retry = jobs.try_recv().unwrap();
        assert_eq!(retry.query, a.id);
        app.dispatch(Action::Enqueue(manual.clone(), true), &ctx);
        commands.try_iter().for_each(drop);
        results
            .send(SearchResult {
                token: retry.token,
                result: Ok(vec![a, manual.clone(), related.clone()]),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.current, Some(manual));
        assert_eq!(
            app.radio.as_ref().unwrap().upcoming,
            VecDeque::from([related.clone()])
        );
        assert!(commands.try_iter().any(|command| matches!(command, PlayerCommand::PrepareNext { track: Some(track), .. } if track == related)));
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
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::PrepareNext { track: Some(track), .. } if track == b)
        );
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
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::PrepareNext { following, .. } if following == vec![c.clone()])
        );
        app.dispatch(Action::StartRadio(a), &ctx);
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::PrepareNext { following, .. } if following.is_empty())
        );
        assert!(commands.try_recv().is_err());
        let job = jobs.recv().unwrap();
        results
            .send(SearchResult {
                token: job.token,
                result: Ok(vec![c.clone()]),
            })
            .unwrap();
        app.receive();
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::PrepareNext { following, .. } if following == vec![c.clone()])
        );
        for expected in [b, c] {
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event: PlaybackEvent::Ended,
                })
                .unwrap();
            app.receive();
            assert_eq!(app.current, Some(expected));
            assert!(matches!(
                commands.recv().unwrap(),
                PlayerCommand::Play { .. }
            ));
            assert!(
                commands
                    .try_iter()
                    .all(|command| matches!(command, PlayerCommand::PrepareNext { .. }))
            );
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
        assert!(app.can_queue_move(0, 1));
        assert!(app.can_queue_move(1, -1));
        app.dispatch(Action::StopRadio, &ctx);
        assert_eq!(app.queue, VecDeque::from([b]));
        assert!(
            commands
                .try_iter()
                .all(|command| matches!(command, PlayerCommand::PrepareNext { .. }))
        );
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
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::PrepareNext { track: Some(track), .. } if track == b)
        );
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
        // The hidden-window path drains events without constructing the UI.
        app.receive();
        assert_eq!(app.current, Some(b));
        assert!(app.queue.is_empty());
        assert!(app.player_rx.try_recv().is_err());
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::Play { .. }
        ));
    }
    #[test]
    fn collection_song_selection_queues_the_remaining_tracks_and_advances_after_navigation() {
        for playlist in [false, true] {
            for input in ["cover", "row", "keyboard"] {
                let (mut app, commands, events, ctx) = fixture();
                ctx.enable_accesskit();
                let mut tracks: Vec<_> =
                    ["dQw4w9WgXcQ", "hTWKbfoikeg", "4NRXx6U8ABQ", "JGwWNGJdvx8"]
                        .into_iter()
                        .map(track)
                        .collect();
                for (track, title) in
                    tracks
                        .iter_mut()
                        .zip(["Speak to Me", "Breathe", "On the Run", "Time"])
                {
                    track.title = title.into();
                }
                app.view = View::Collection;
                if playlist {
                    app.playlists.push(Playlist {
                        id: 7,
                        title: "Album playlist".into(),
                        tracks: tracks.clone(),
                    });
                    app.playlist_id = Some(7);
                } else {
                    app.collection = Some(Collection {
                        id: "MPREalbum".into(),
                        title: "Album".into(),
                        subtitle: "Artist".into(),
                        kind: CollectionKind::Album,
                        thumbnail: None,
                        params: None,
                    });
                    app.detail.tracks = tracks.clone();
                }
                app.queue.push_back(track("Eq8ZiZ9xiLw"));
                app.queue_selected = Some(0);
                app.radio = Some(Radio::new(track("Eq8ZiZ9xiLw")));
                app.selected = Some(1);
                app.filter = "Breathe".into();
                let render = |app: &mut MusicApp, events| {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(1280., 900.),
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
                    output
                };
                let output = render(&mut app, vec![]);
                let event = if input == "keyboard" {
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }
                } else {
                    let label = if input == "cover" {
                        "Play Breathe"
                    } else {
                        "Breathe · Artist"
                    };
                    let id = output
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .unwrap()
                        .nodes
                        .iter()
                        .find(|(_, node)| node.label() == Some(label))
                        .unwrap()
                        .0;
                    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Click,
                        target_tree: egui::accesskit::TreeId::ROOT,
                        target_node: id,
                        data: None,
                    })
                };
                render(&mut app, vec![event]);
                assert_eq!(
                    app.current.as_ref(),
                    Some(&tracks[1]),
                    "{input}, playlist={playlist}"
                );
                assert_eq!(app.queue.iter().cloned().collect::<Vec<_>>(), tracks[2..]);
                assert!(app.radio.is_none());
                assert!(app.queue_selected.is_none());
                let sent: Vec<_> = commands.try_iter().collect();
                assert!(sent.iter().any(|command| matches!(command, PlayerCommand::Play { track, .. } if track == &tracks[1])));
                assert!(sent.iter().any(|command| matches!(command, PlayerCommand::PrepareNext { track: Some(next), following, .. } if next == &tracks[2] && following == &tracks[3..])));
                app.dispatch(Action::Navigate(View::Search), &ctx);
                for next in &tracks[2..] {
                    events
                        .send(PlayerEvent {
                            generation: app.generation,
                            event: PlaybackEvent::Ended,
                        })
                        .unwrap();
                    app.receive();
                    assert_eq!(app.current.as_ref(), Some(next));
                }
                events
                    .send(PlayerEvent {
                        generation: app.generation,
                        event: PlaybackEvent::Ended,
                    })
                    .unwrap();
                app.receive();
                assert!(app.queue.is_empty());
                assert_eq!(app.playback, Playback::Stopped);
                assert_eq!(app.radio.as_ref().unwrap().seed, tracks[3]);
                assert!(app.radio.as_ref().unwrap().waiting);
            }
        }
    }

    #[test]
    fn collection_playback_starts_at_the_selected_occurrence_and_ignores_invalid_indices() {
        let (mut app, commands, _, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("4NRXx6U8ABQ");
        app.view = View::Collection;
        app.detail.tracks = vec![a.clone(), b, a.clone(), c.clone()];
        app.dispatch(Action::PlayCollectionTrack(2), &ctx);
        assert_eq!(app.current, Some(a));
        assert_eq!(app.queue, VecDeque::from([c]));
        commands.try_iter().for_each(drop);
        app.dispatch(Action::PlayCollectionTrack(4), &ctx);
        assert!(commands.try_recv().is_err());
        assert_eq!(app.queue.len(), 1);
    }

    #[test]
    fn preparation_window_is_bounded_and_tail_edits_preserve_the_handoff_ticket() {
        let (mut app, commands, _, ctx) = fixture();
        let tracks: Vec<_> = [
            "dQw4w9WgXcQ",
            "hTWKbfoikeg",
            "4NRXx6U8ABQ",
            "Eq8ZiZ9xiLw",
            "-0kcet4aPpQ",
            "JGwWNGJdvx8",
        ]
        .into_iter()
        .map(track)
        .collect();
        app.dispatch(Action::PlayTracks(tracks.clone()), &ctx);
        commands.recv().unwrap();
        let PlayerCommand::PrepareNext {
            track: head,
            following,
            revision,
            ..
        } = commands.recv().unwrap()
        else {
            panic!()
        };
        assert_eq!(head, Some(tracks[1].clone()));
        assert_eq!(following, tracks[2..5]);
        // Editing outside the preparation window causes no network work.
        app.dispatch(Action::QueueRemove(4), &ctx);
        assert!(commands.try_recv().is_err());
        app.dispatch(Action::QueueRemove(1), &ctx);
        let PlayerCommand::PrepareNext {
            track: head,
            following,
            revision: same,
            ..
        } = commands.recv().unwrap()
        else {
            panic!()
        };
        assert_eq!(head, Some(tracks[1].clone()));
        assert_eq!(same, revision);
        assert_eq!(following, tracks[3..5]);
    }
    #[test]
    fn native_handoffs_consume_one_occurrence_and_preserve_history() {
        let (mut app, commands, events, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        app.dispatch(
            Action::PlayTracks(vec![a.clone(), b.clone(), b.clone()]),
            &ctx,
        );
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::Play { .. }
        ));
        let PlayerCommand::PrepareNext { revision, .. } = commands.recv().unwrap() else {
            panic!("next track wasn't prepared")
        };
        let parent = app.generation;
        // Tail edits preserve the preloaded stream and its scheduling ticket.
        app.dispatch(Action::Enqueue(a.clone(), false), &ctx);
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::PrepareNext { revision: same_revision, following, .. } if same_revision == revision && following == vec![b.clone(), a.clone()])
        );
        assert!(commands.try_recv().is_err());
        events
            .send(PlayerEvent {
                generation: parent,
                event: PlaybackEvent::Transition {
                    track: b.clone(),
                    revision,
                },
            })
            .unwrap();
        events
            .send(PlayerEvent {
                generation: parent.wrapping_add(1),
                event: PlaybackEvent::Ready,
            })
            .unwrap();
        // A duplicate delivery must not consume the second occurrence.
        events
            .send(PlayerEvent {
                generation: parent,
                event: PlaybackEvent::Transition {
                    track: b.clone(),
                    revision,
                },
            })
            .unwrap();
        app.receive();
        assert_eq!(app.current, Some(b.clone()));
        assert_eq!(app.queue, VecDeque::from([b.clone(), a.clone()]));
        assert_eq!(app.history, vec![a]);
        assert_eq!(app.recent.first(), Some(&b));
        assert_eq!(app.playback, Playback::Playing);
        assert!(
            commands
                .try_iter()
                .all(|command| matches!(command, PlayerCommand::PrepareNext { .. }))
        );
    }
    #[test]
    fn native_radio_handoff_keeps_manual_priority_and_continuation() {
        let (mut app, commands, events, ctx) = fixture();
        let (tx, _jobs) = mpsc::channel();
        app.radio_tx = tx;
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.dispatch(Action::StartRadio(a.clone()), &ctx);
        commands.recv().unwrap();
        app.radio.as_mut().unwrap().upcoming.push_back(c.clone());
        app.dispatch(Action::Enqueue(b.clone(), false), &ctx);
        for expected in [b.clone(), c.clone()] {
            let PlayerCommand::PrepareNext {
                generation,
                revision,
                track: Some(prepared),
                ..
            } = commands.recv().unwrap()
            else {
                panic!("missing radio prefetch")
            };
            assert_eq!(prepared, expected);
            events
                .send(PlayerEvent {
                    generation,
                    event: PlaybackEvent::Transition {
                        track: expected.clone(),
                        revision,
                    },
                })
                .unwrap();
            events
                .send(PlayerEvent {
                    generation: generation.wrapping_add(1),
                    event: PlaybackEvent::Ready,
                })
                .unwrap();
            app.receive();
            assert_eq!(app.current, Some(expected.clone()));
            assert_eq!(
                app.radio.as_ref().unwrap().continuation,
                if expected == b { a.clone() } else { c.clone() }
            );
        }
        assert_eq!(app.history, vec![a, b]);
        assert!(app.queue.is_empty());
        assert!(app.radio.as_ref().unwrap().upcoming.is_empty());
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn queue_edit_crossing_handoff_plays_the_current_choice() {
        let (mut app, commands, events, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.dispatch(Action::PlayTracks(vec![a, b.clone(), c.clone()]), &ctx);
        commands.recv().unwrap();
        let PlayerCommand::PrepareNext {
            revision,
            generation,
            ..
        } = commands.recv().unwrap()
        else {
            panic!()
        };
        app.dispatch(Action::QueueRemove(0), &ctx);
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::PrepareNext { track: Some(track), .. } if track == c)
        );
        events
            .send(PlayerEvent {
                generation,
                event: PlaybackEvent::Transition { track: b, revision },
            })
            .unwrap();
        app.receive();
        assert_eq!(app.current, Some(c.clone()));
        assert!(app.queue.is_empty());
        assert!(
            matches!(commands.recv().unwrap(), PlayerCommand::Play { track, .. } if track == c)
        );
        for event in [
            PlaybackEvent::Ready,
            PlaybackEvent::Duration(383.),
            PlaybackEvent::Position(4.),
        ] {
            events
                .send(PlayerEvent {
                    generation: generation.wrapping_add(1),
                    event,
                })
                .unwrap();
        }
        app.receive();
        assert_eq!(app.playback, Playback::Loading);
        assert_eq!(app.duration, None);
        assert_eq!(app.position, 0.);
        app.dispatch(Action::Stop, &ctx);
        events
            .send(PlayerEvent {
                generation,
                event: PlaybackEvent::Transition { track: c, revision },
            })
            .unwrap();
        app.receive();
        assert_eq!(app.playback, Playback::Stopped);
    }
    #[test]
    fn playback_preparation_is_hidden_and_pause_remains_available() {
        let (mut app, commands, events, ctx) = fixture();
        ctx.enable_accesskit();
        app.dispatch(Action::Play(track("Eq8ZiZ9xiLw")), &ctx);
        commands.try_iter().for_each(drop);
        let render = |app: &mut MusicApp| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280., 800.),
                    )),
                    ..Default::default()
                },
                |ui| {
                    crate::ui::show(app, ui);
                },
            );
            output.textures_delta.clear();
            output
        };
        for recovering in [false, true] {
            if recovering {
                events
                    .send(PlayerEvent {
                        generation: app.generation,
                        event: PlaybackEvent::Recovering,
                    })
                    .unwrap();
                app.receive();
            }
            let output = render(&mut app);
            let nodes = &output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes;
            assert!(
                nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Pause · Space") && !node.is_disabled())
            );
            let text = format!("{:?}", output.shapes);
            for hidden in [
                "Loading audio",
                "Cancel loading",
                "Reconnecting",
                "Buffering",
            ] {
                assert!(!text.contains(hidden));
            }
            app.dispatch(Action::TogglePlayback, &ctx);
            assert!(matches!(
                commands.recv().unwrap(),
                PlayerCommand::SetPaused(true)
            ));
            let output = render(&mut app);
            assert!(
                output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some("Play · Space") && !node.is_disabled())
            );
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event: PlaybackEvent::Ready,
                })
                .unwrap();
            app.receive();
            assert_eq!(app.playback, Playback::Paused);
            app.dispatch(Action::TogglePlayback, &ctx);
            assert!(matches!(
                commands.recv().unwrap(),
                PlayerCommand::SetPaused(false)
            ));
        }
    }
    #[test]
    fn recovery_preserves_queue_and_position_then_restores_pause() {
        let (mut app, commands, events, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        app.dispatch(Action::PlayTracks(vec![a.clone(), b.clone()]), &ctx);
        commands.try_iter().for_each(drop);
        app.position = 32.;
        for event in [
            PlaybackEvent::Recovering,
            PlaybackEvent::Ready,
            PlaybackEvent::Paused(true),
        ] {
            events
                .send(PlayerEvent {
                    generation: app.generation,
                    event,
                })
                .unwrap();
        }
        app.receive();
        assert_eq!(app.current, Some(a));
        assert_eq!(app.queue, VecDeque::from([b]));
        assert_eq!(app.position, 32.);
        assert_eq!(app.playback, Playback::Paused);
        assert!(!app.buffering);
        assert!(commands.try_recv().is_err());
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
    fn playlists_edit_order_preserve_duplicates_and_play_as_a_collection() {
        let (mut app, commands, _, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.dispatch(
            Action::CreatePlaylist("  Mix  ".into(), vec![a.clone(), b.clone(), a.clone()]),
            &ctx,
        );
        let id = app.playlists[0].id;
        assert_eq!(app.playlists[0].title, "Mix");
        app.dispatch(Action::RenamePlaylist(id, " ".into()), &ctx);
        assert_eq!(app.playlists[0].title, "Mix");
        app.dispatch(Action::PlaylistAdd(id, c.clone(), 1), &ctx);
        app.dispatch(Action::PlaylistMove(id, 0, 4), &ctx);
        assert_eq!(
            app.playlists[0].tracks,
            [c.clone(), b.clone(), a.clone(), a.clone()]
        );
        app.dispatch(Action::PlaylistRemove(id, 1), &ctx);
        app.dispatch(Action::OpenPlaylist(id), &ctx);
        assert_eq!(app.active_tracks(), [c.clone(), a.clone(), a.clone()]);
        app.dispatch(Action::PlayTracks(app.active_tracks().to_vec()), &ctx);
        assert_eq!(app.current, Some(c));
        assert_eq!(app.queue, VecDeque::from([a.clone(), a]));
        assert!(matches!(
            commands.recv().unwrap(),
            PlayerCommand::Play { .. }
        ));
        app.dispatch(Action::DeletePlaylist(id), &ctx);
        assert_eq!(app.view, View::Playlists);
        assert!(app.playlists.is_empty());
    }
    #[test]
    fn queue_drops_insert_stably_across_radio_and_reject_stale_drags() {
        let (mut app, commands, _, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        let c = track("JGwWNGJdvx8");
        app.current = Some(a.clone());
        app.queue = [a.clone(), b.clone(), c.clone()].into();
        let drag = TrackDrag {
            track: a.clone(),
            origin: DragOrigin::Queue {
                index: 0,
                revision: app.queue_revision,
            },
        };
        app.dispatch(Action::QueueDrop(drag.clone(), 3), &ctx);
        assert_eq!(app.queue, VecDeque::from([b.clone(), c.clone(), a.clone()]));
        app.dispatch(Action::QueueDrop(drag, 0), &ctx);
        assert_eq!(app.queue, VecDeque::from([b.clone(), c.clone(), a.clone()]));
        app.radio = Some(Radio::new(a.clone()));
        app.radio.as_mut().unwrap().upcoming = [b.clone(), c.clone()].into();
        app.queue = [a.clone()].into();
        app.dispatch(
            Action::QueueDrop(
                TrackDrag {
                    track: c.clone(),
                    origin: DragOrigin::Queue {
                        index: 2,
                        revision: app.queue_revision,
                    },
                },
                0,
            ),
            &ctx,
        );
        assert_eq!(app.queue, VecDeque::from([c.clone(), a.clone(), b.clone()]));
        assert!(app.radio.as_ref().unwrap().upcoming.is_empty());
        app.dispatch(
            Action::QueueDrop(
                TrackDrag {
                    track: b.clone(),
                    origin: DragOrigin::Track,
                },
                1,
            ),
            &ctx,
        );
        assert_eq!(app.queue, VecDeque::from([c, b.clone(), a.clone(), b]));
        assert_eq!(app.current, Some(a));
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn playlist_drops_move_within_copy_across_and_ignore_outdated_positions() {
        let (mut app, _, _, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let b = track("hTWKbfoikeg");
        app.dispatch(
            Action::CreatePlaylist("One".into(), vec![a.clone(), b.clone()]),
            &ctx,
        );
        app.dispatch(Action::CreatePlaylist("Two".into(), vec![]), &ctx);
        let first = app.playlists[0].id;
        let second = app.playlists[1].id;
        let drag = TrackDrag {
            track: a.clone(),
            origin: DragOrigin::Playlist {
                id: first,
                index: 0,
                revision: app.playlist_revision,
            },
        };
        app.dispatch(Action::PlaylistDrop(first, drag.clone(), 2), &ctx);
        assert_eq!(app.playlists[0].tracks, [b.clone(), a.clone()]);
        app.dispatch(Action::PlaylistDrop(first, drag.clone(), 0), &ctx);
        assert_eq!(app.playlists[0].tracks, [b, a.clone()]);
        app.dispatch(Action::PlaylistDrop(second, drag, 0), &ctx);
        assert_eq!(app.playlists[1].tracks, [a]);
        assert_eq!(app.playlists[0].tracks.len(), 2);
    }
    #[test]
    fn catalog_results_cancel_on_navigation_and_collection_pages_keep_repeats() {
        let (mut app, _, _, ctx) = fixture();
        let a = track("dQw4w9WgXcQ");
        let (tx, rx) = mpsc::channel();
        app.search_rx = rx;
        app.query = "Example".into();
        app.dispatch(Action::Search, &ctx);
        let stale = app.search_token;
        app.dispatch(Action::Navigate(View::Library), &ctx);
        tx.send(CatalogResult {
            token: stale,
            detail: false,
            append: false,
            result: Ok(vec![a.clone()].into()),
        })
        .unwrap();
        app.receive();
        assert!(app.results.is_empty());
        tx.send(CatalogResult {
            token: app.search_token,
            detail: true,
            append: false,
            result: Ok(vec![a.clone()].into()),
        })
        .unwrap();
        tx.send(CatalogResult {
            token: app.search_token,
            detail: true,
            append: true,
            result: Ok(vec![a.clone()].into()),
        })
        .unwrap();
        app.receive();
        assert_eq!(app.detail.tracks, [a.clone(), a]);
    }
    #[test]
    fn saved_albums_are_available_immediately_and_back_restores_collection() {
        let (mut app, _, _, ctx) = fixture();
        let collection = Collection {
            id: "MPREalbum".into(),
            title: "Album".into(),
            subtitle: "Artist".into(),
            kind: CollectionKind::Album,
            thumbnail: None,
            params: None,
        };
        let a = track("dQw4w9WgXcQ");
        app.albums.push(SavedAlbum {
            collection: collection.clone(),
            tracks: vec![a.clone()],
        });
        app.dispatch(Action::OpenCollection(collection.clone()), &ctx);
        assert_eq!(app.active_tracks(), std::slice::from_ref(&a));
        app.dispatch(Action::CreatePlaylist("Mix".into(), vec![a]), &ctx);
        app.dispatch(Action::OpenPlaylist(app.playlists[0].id), &ctx);
        app.dispatch(Action::Back, &ctx);
        assert_eq!(app.collection, Some(collection));
        assert!(app.playlist_id.is_none());
        assert_eq!(app.detail.tracks.len(), 1);
    }
    #[test]
    fn volume_and_queue_edits_share_snapshots_and_collection_edits_replace_them() {
        let (mut app, _, _, ctx) = fixture();
        let track = track("dQw4w9WgXcQ");
        app.dispatch(
            Action::CreatePlaylist("Mix".into(), vec![track.clone(); 1000]),
            &ctx,
        );
        let first = app.collection_snapshot();
        app.dispatch(Action::Volume(30), &ctx);
        app.dispatch(Action::Enqueue(track.clone(), false), &ctx);
        let unchanged = app.collection_snapshot();
        assert!(Arc::ptr_eq(&first.playlists, &unchanged.playlists));
        app.dispatch(Action::PlaylistRemove(app.playlists[0].id, 0), &ctx);
        let edited = app.collection_snapshot();
        assert!(!Arc::ptr_eq(&first.playlists, &edited.playlists));
        assert_eq!(first.playlists[0].tracks.len(), 1000);
        assert_eq!(edited.playlists[0].tracks.len(), 999);
        app.dispatch(Action::SaveTrack(track.clone()), &ctx);
        app.dispatch(Action::LibraryTab(LibraryTab::Saved), &ctx);
        assert_eq!(app.library, [track]);
        app.dispatch(Action::LibraryTab(LibraryTab::Recent), &ctx);
        assert!(app.library.is_empty());
    }
    #[test]
    fn player_bar_sends_volume_commands_before_pointer_release() {
        for (width, start, midpoint) in [(1280., 1104., 1136.), (960., 814., 836.)] {
            let (mut app, commands, _, ctx) = fixture();
            let frame = |app: &mut MusicApp, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 800.),
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
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(&mut app, vec![]);
            let press = egui::pos2(start, 744.);
            frame(
                &mut app,
                vec![egui::Event::PointerMoved(press), button(press, true)],
            );
            assert_eq!(app.volume, 25);
            assert!(matches!(
                commands.try_recv().unwrap(),
                PlayerCommand::SetVolume(25)
            ));
            frame(
                &mut app,
                vec![egui::Event::PointerMoved(egui::pos2(midpoint, 744.))],
            );
            assert_eq!(app.volume, 50);
            assert!(matches!(
                commands.try_recv().unwrap(),
                PlayerCommand::SetVolume(50)
            ));
            frame(&mut app, vec![button(egui::pos2(midpoint, 744.), false)]);
            assert!(commands.try_recv().is_err());
        }
    }

    #[test]
    fn volume_edits_keep_the_latest_value_and_skip_duplicate_commands() {
        let (mut app, commands, _, ctx) = fixture();
        for volume in [30, 30, 31, 0, 90, 255, 100] {
            app.dispatch(Action::Volume(volume), &ctx);
        }
        let sent: Vec<_> = commands
            .try_iter()
            .map(|command| {
                let PlayerCommand::SetVolume(volume) = command else {
                    panic!("unexpected command")
                };
                volume
            })
            .collect();
        assert_eq!(sent, [30, 31, 0, 90, 100]);
        assert_eq!(app.volume, 100);
        app.dispatch(Action::Mute, &ctx);
        app.dispatch(Action::Mute, &ctx);
        assert_eq!(app.volume, 100);
        // Dragging away from mute establishes the volume restored by the mute button.
        app.dispatch(Action::Volume(0), &ctx);
        app.dispatch(Action::Volume(23), &ctx);
        app.dispatch(Action::Mute, &ctx);
        app.dispatch(Action::Mute, &ctx);
        assert_eq!(app.volume, 23);
    }

    #[test]
    fn seek_bar_edits_resume_position_before_playback_and_during_playback() {
        for playback in [
            Playback::Stopped,
            Playback::Error,
            Playback::Loading,
            Playback::Playing,
            Playback::Paused,
        ] {
            let (mut app, commands, _, ctx) = fixture();
            ctx.enable_accesskit();
            app.current = Some(track("dQw4w9WgXcQ"));
            app.playback = playback;
            app.position = 65.;
            // A restored session only has the catalog duration, no mpv metadata.
            assert!(app.duration.is_none());
            assert!(!app.seekable);
            let render = |app: &mut MusicApp, events| {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1280., 800.),
                        )),
                        events,
                        ..Default::default()
                    },
                    |ui| crate::ui::show(app, ui),
                );
                output.textures_delta.clear();
                output
            };
            let output = render(&mut app, vec![]);
            let (id, node) = output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("Seek"))
                .unwrap();
            assert!(!node.is_disabled(), "seek disabled for {playback:?}");
            commands.try_iter().for_each(drop);
            render(
                &mut app,
                vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::SetValue,
                        target_tree: egui::accesskit::TreeId::ROOT,
                        target_node: *id,
                        data: Some(egui::accesskit::ActionData::NumericValue(73.)),
                    },
                )],
            );
            assert_eq!(app.position, 73., "seek ignored for {playback:?}");
            assert_eq!(app.playback, playback);
            if matches!(playback, Playback::Stopped | Playback::Error) {
                assert!(commands.try_recv().is_err());
                app.dispatch(Action::TogglePlayback, &ctx);
                assert!(matches!(
                    commands.recv().unwrap(),
                    PlayerCommand::Play { position: 73., .. }
                ));
            } else {
                assert!(matches!(commands.recv().unwrap(), PlayerCommand::Seek(73.)));
            }
        }
    }

    #[test]
    fn seek_commands_use_the_clamped_position_and_ignore_non_finite_values() {
        let (mut app, commands, _, ctx) = fixture();
        app.current = Some(track("dQw4w9WgXcQ"));
        app.playback = Playback::Playing;
        for (requested, expected) in [(-10., 0.), (250., 180.)] {
            app.dispatch(Action::Seek(requested), &ctx);
            assert_eq!(app.position, expected);
            assert!(matches!(
                commands.recv().unwrap(),
                PlayerCommand::Seek(position) if position == expected
            ));
        }
        for requested in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            app.dispatch(Action::Seek(requested), &ctx);
            assert_eq!(app.position, 180.);
            assert!(commands.try_recv().is_err());
        }
    }

    #[test]
    fn mute_restores_volume_and_seek_needs_a_current_track() {
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
        app.current = Some(track("dQw4w9WgXcQ"));
        app.playback = Playback::Paused;
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

use super::{
    icons::Icon,
    theme::*,
    widgets::{self, Artwork, Row},
};
use crate::{
    app::{Action, MusicApp, Playback, PlaylistEditor, TrackDrag, View},
    catalog::SearchFilter,
    model::{Collection, CollectionKind},
};
use eframe::egui::{self, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

pub fn auto_scroll(ui: &mut Ui) {
    if egui::DragAndDrop::has_payload_of_type::<TrackDrag>(ui.ctx())
        && let Some(pointer) = ui.ctx().pointer_interact_pos()
    {
        let rect = ui.clip_rect();
        if pointer.x >= rect.left() && pointer.x <= rect.right() {
            let speed = if pointer.y < rect.top() + 40. {
                420.
            } else if pointer.y > rect.bottom() - 40. {
                -420.
            } else {
                0.
            };
            if speed != 0. {
                let dt = ui.input(|i| i.stable_dt).min(0.05);
                ui.scroll_with_delta(vec2(0., speed * dt));
                ui.ctx().request_repaint();
            }
        }
    }
}

pub fn drop_zone(
    ui: &mut Ui,
    label: &str,
    action: impl Fn(TrackDrag) -> Action,
    actions: &mut Vec<Action>,
) {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 44.), Sense::hover());
    let hovering = response.dnd_hover_payload::<TrackDrag>().is_some();
    ui.painter().rect_stroke(
        rect.shrink(2.),
        8,
        Stroke::new(1., if hovering { ACCENT } else { LINE }),
        egui::StrokeKind::Inside,
    );
    widgets::text(
        ui,
        rect.shrink2(vec2(12., 0.)),
        label,
        12.,
        if hovering { ACCENT } else { MUTED },
    );
    if let Some(drag) = response.dnd_release_payload::<TrackDrag>() {
        actions.push(action((*drag).clone()));
    }
}

fn collection_row(
    ui: &mut Ui,
    artwork: &mut Artwork,
    collection: &Collection,
    actions: &mut Vec<Action>,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let response = ui.interact(rect, ui.id().with(&collection.id), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("Open {} {}", collection.kind.label(), collection.title),
        )
    });
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 8, HOVER);
    }
    let cover = Rect::from_min_size(rect.min + vec2(8., 8.), vec2(48., 48.));
    if let Some(url) = &collection.thumbnail {
        artwork.paint_url(
            ui,
            &collection.id,
            url,
            cover,
            if collection.kind == CollectionKind::Artist {
                24
            } else {
                6
            },
        );
    } else {
        widgets::placeholder(ui, cover, 6, Icon::Music);
    }
    let title = Rect::from_min_max(
        rect.min + vec2(72., 8.),
        pos2(rect.right() - 16., rect.top() + 32.),
    );
    widgets::text(ui, title, &collection.title, 14., TEXT);
    widgets::text(
        ui,
        title.translate(vec2(0., 24.)),
        &format!("{} · {}", collection.kind.label(), collection.subtitle),
        12.,
        MUTED,
    );
    if response.clicked() {
        actions.push(Action::OpenCollection(collection.clone()));
    }
    response.on_hover_text(&collection.title);
}

fn reveal(
    ui: &Ui,
    scroll: egui::ScrollArea,
    id: impl std::hash::Hash + std::fmt::Debug,
    row: Option<usize>,
    requested: &mut bool,
    height: f32,
) -> egui::ScrollArea {
    if !std::mem::take(requested) {
        return scroll;
    }
    let Some(row) = row else {
        return scroll;
    };
    let offset = egui::scroll_area::State::load(ui.ctx(), ui.make_persistent_id(id))
        .map_or(0., |s| s.offset.y);
    let y = row as f32 * 64.;
    scroll.vertical_scroll_offset(if y < offset {
        y
    } else if y + 64. > offset + height {
        y + 64. - height
    } else {
        offset
    })
}

pub fn search(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    widgets::heading(ui, "Search", "Find songs, albums, artists, and playlists.");
    ui.add_space(16.);
    ui.horizontal_wrapped(|ui| {
        for filter in [
            SearchFilter::All,
            SearchFilter::Songs,
            SearchFilter::Albums,
            SearchFilter::Artists,
            SearchFilter::Playlists,
        ] {
            if ui
                .selectable_label(app.search_filter == filter, filter.label())
                .clicked()
            {
                actions.push(Action::SearchFilter(filter));
            }
        }
    });
    ui.add_space(12.);
    if app.searching {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Searching…");
            if ui.button("Cancel").clicked() {
                actions.push(Action::CancelSearch);
            }
        });
    }
    if let Some(error) = &app.search_error {
        ui.label(RichText::new(error).color(ERROR));
        if ui.button("Try again").clicked() {
            actions.push(
                if app.results.is_empty() && app.search_collections.is_empty() {
                    Action::Search
                } else {
                    Action::SearchMore
                },
            );
        }
    }
    let collections = &app.search_collections;
    let count = collections.len() + app.results.len();
    if count == 0 && !app.searching && app.search_error.is_none() {
        widgets::empty(
            ui,
            Icon::Search,
            if app.search_done {
                "No matches"
            } else {
                "Find your next favorite"
            },
            if app.search_done {
                "Try another title or artist."
            } else {
                "Enter a song, artist, album, or playlist above."
            },
            "",
        );
        return;
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(&app.submitted_query).font(bold(20.)));
        if app.search_continuation.is_some()
            && ui
                .add_enabled(!app.searching, egui::Button::new("Load more results"))
                .clicked()
        {
            actions.push(Action::SearchMore);
        }
    });
    ui.add_space(8.);
    let id = (
        "catalog_search",
        app.search_filter.label(),
        &app.submitted_query,
    );
    let scroll = reveal(
        ui,
        egui::ScrollArea::vertical()
            .id_salt(id)
            .auto_shrink([false, false]),
        id,
        app.selected,
        &mut app.reveal_selection,
        ui.available_height(),
    );
    scroll.show_rows(ui, 64., count, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.;
        for index in range {
            if index >= app.results.len() {
                collection_row(
                    ui,
                    &mut app.artwork,
                    &collections[index - app.results.len()],
                    actions,
                );
            } else {
                let i = index;
                let track = &app.results[i];
                widgets::track_row(
                    ui,
                    &mut app.artwork,
                    Row {
                        track,
                        index: i,
                        current: app.current.as_ref().is_some_and(|t| t.id == track.id),
                        playing: app.playback == Playback::Playing,
                        liked: app.favorites.iter().any(|t| t.id == track.id),
                        selected: app.selected == Some(i),
                        queue: false,
                        collection: false,
                        radio: false,
                        queue_range: 0..0,
                        saved: app.saved_tracks.iter().any(|t| t.id == track.id),
                        queue_revision: app.queue_revision,
                        playlist: None,
                    },
                    actions,
                );
            }
        }
    });
}

pub fn library(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    let (title, description) = match app.view {
        View::Albums => ("Albums", "Albums saved to your library."),
        View::Artists => ("Artists", "Artists you follow."),
        _ => ("Playlists", "Your collections, in your order."),
    };
    widgets::heading(ui, title, description);
    ui.add_space(16.);
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.filter)
                .hint_text("Filter your collection")
                .desired_width(200.),
        );
        if app.view == View::Playlists && ui.button("New playlist").clicked() {
            app.playlist_editor = Some(PlaylistEditor::Create {
                title: String::new(),
                tracks: Vec::new(),
            });
        }
    });
    ui.add_space(16.);
    let filter = app.filter.trim().to_lowercase();
    if app.view == View::Playlists {
        let indices: Vec<_> = app
            .playlists
            .iter()
            .enumerate()
            .filter(|(_, p)| p.title.to_lowercase().contains(&filter))
            .map(|(i, _)| i)
            .collect();
        if indices.is_empty() {
            ui.label(if app.playlists.is_empty() {
                "No playlists yet. Create one or import a playlist from search."
            } else {
                "No matching playlists."
            });
        }
        egui::ScrollArea::vertical()
            .id_salt("playlist_library")
            .auto_shrink([false, false])
            .show_rows(ui, 72., indices.len(), |ui, range| {
                for index in range {
                    let playlist = &app.playlists[indices[index]];
                    ui.push_id(playlist.id, |ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(vec2(ui.available_width(), 72.), Sense::hover());
                        let response = ui.interact(rect, ui.id().with("open"), Sense::click());
                        if response.hovered() || response.dnd_hover_payload::<TrackDrag>().is_some()
                        {
                            ui.painter().rect_filled(rect, 8, HOVER);
                        }
                        let shell = rect.shrink2(vec2(0., 3.));
                        widgets::gradient(ui.painter(), shell, 10., SURFACE, BG);
                        ui.painter().rect_stroke(
                            shell,
                            10,
                            Stroke::new(1., LINE),
                            egui::StrokeKind::Inside,
                        );
                        let cover = Rect::from_min_size(rect.min + vec2(8., 8.), vec2(48., 48.));
                        if let Some(track) = playlist.tracks.first() {
                            app.artwork.paint(ui, track, cover, 6);
                        } else {
                            widgets::placeholder(ui, cover, 6, Icon::Library);
                        }
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                format!("Open playlist {}", playlist.title),
                            )
                        });
                        widgets::text(
                            ui,
                            Rect::from_min_max(
                                rect.min + vec2(72., 8.),
                                pos2(rect.right() - 16., rect.top() + 32.),
                            ),
                            &playlist.title,
                            14.,
                            TEXT,
                        );
                        widgets::text(
                            ui,
                            Rect::from_min_max(
                                rect.min + vec2(72., 32.),
                                pos2(rect.right() - 16., rect.top() + 56.),
                            ),
                            &format!("{} songs · Drop a track to add it", playlist.tracks.len()),
                            12.,
                            MUTED,
                        );
                        if response.clicked() {
                            actions.push(Action::OpenPlaylist(playlist.id));
                        }
                        if let Some(drag) = response.dnd_release_payload::<TrackDrag>() {
                            actions.push(Action::PlaylistDrop(
                                playlist.id,
                                (*drag).clone(),
                                playlist.tracks.len(),
                            ));
                        }
                        response.context_menu(|ui| {
                            if ui.button("Play").clicked() {
                                actions.push(Action::PlayTracks(playlist.tracks.clone()));
                                ui.close();
                            }
                            if ui.button("Add to queue").clicked() {
                                actions.push(Action::QueueTracks(playlist.tracks.clone()));
                                ui.close();
                            }
                            if ui.button("Rename…").clicked() {
                                app.playlist_editor = Some(PlaylistEditor::Rename {
                                    id: playlist.id,
                                    title: playlist.title.clone(),
                                });
                                ui.close();
                            }
                            if ui.button("Delete…").clicked() {
                                app.playlist_editor = Some(PlaylistEditor::Delete {
                                    id: playlist.id,
                                    title: playlist.title.clone(),
                                });
                                ui.close();
                            }
                        });
                    });
                }
            });
    } else {
        let entries: Vec<&Collection> = if app.view == View::Albums {
            app.albums.iter().map(|a| &a.collection).collect()
        } else {
            app.artists.iter().collect()
        };
        let entries: Vec<_> = entries
            .into_iter()
            .filter(|c| {
                c.title.to_lowercase().contains(&filter)
                    || c.subtitle.to_lowercase().contains(&filter)
            })
            .collect();
        if entries.is_empty() && ui.button("Find music to save").clicked() {
            actions.push(Action::FocusSearch);
        }
        let columns = ((ui.available_width() + 16.) / 184.).floor().max(1.) as usize;
        let width = (ui.available_width() - (columns - 1) as f32 * 16.) / columns as f32;
        egui::ScrollArea::vertical()
            .id_salt(title)
            .auto_shrink([false, false])
            .show_rows(
                ui,
                width + 78.,
                entries.len().div_ceil(columns),
                |ui, range| {
                    for row in range {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 16.;
                            for collection in entries.iter().skip(row * columns).take(columns) {
                                let fallback = app
                                    .albums
                                    .iter()
                                    .find(|a| a.collection.id == collection.id)
                                    .and_then(|a| a.tracks.first());
                                ui.push_id(&collection.id, |ui| {
                                    let (r, _) = ui.allocate_exact_size(
                                        vec2(width, width + 70.),
                                        Sense::hover(),
                                    );
                                    let response = ui.interact(
                                        r,
                                        ui.id().with("collection_card"),
                                        Sense::click(),
                                    );
                                    response.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::Button,
                                            true,
                                            format!(
                                                "Open {} {}",
                                                collection.kind.label(),
                                                collection.title
                                            ),
                                        )
                                    });
                                    if response.hovered() || response.has_focus() {
                                        ui.painter().rect_filled(r, 12, SURFACE);
                                    }
                                    let cover = Rect::from_min_size(r.min, vec2(width, width));
                                    let radius = if collection.kind == CollectionKind::Artist {
                                        (width / 2.).min(255.) as u8
                                    } else {
                                        10
                                    };
                                    if let Some(url) = &collection.thumbnail {
                                        app.artwork.paint_url(
                                            ui,
                                            &collection.id,
                                            url,
                                            cover,
                                            radius,
                                        );
                                    } else if let Some(track) = fallback {
                                        app.artwork.paint(ui, track, cover, radius);
                                    } else {
                                        widgets::placeholder(ui, cover, radius, Icon::Music);
                                    }
                                    widgets::text(
                                        ui,
                                        Rect::from_min_size(
                                            r.min + vec2(4., width + 10.),
                                            vec2(width - 8., 24.),
                                        ),
                                        &collection.title,
                                        14.,
                                        TEXT,
                                    );
                                    widgets::text(
                                        ui,
                                        Rect::from_min_size(
                                            r.min + vec2(4., width + 36.),
                                            vec2(width - 8., 22.),
                                        ),
                                        &collection.subtitle,
                                        12.,
                                        MUTED,
                                    );
                                    if response.clicked() {
                                        actions.push(Action::OpenCollection((*collection).clone()));
                                    }
                                    response.on_hover_text(&collection.title);
                                });
                            }
                        });
                    }
                },
            );
    }
}

pub fn detail(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    if ui.button("← Back").clicked() {
        actions.push(Action::Back);
    }
    ui.add_space(12.);
    let playlist = app
        .playlist_id
        .and_then(|id| app.playlists.iter().find(|p| p.id == id));
    let title = playlist
        .map(|p| p.title.as_str())
        .or_else(|| app.collection.as_ref().map(|c| c.title.as_str()))
        .unwrap_or("Collection");
    let subtitle = app
        .collection
        .as_ref()
        .map_or("Your playlist", |c| c.subtitle.as_str());
    let source = playlist.map_or(&app.detail.tracks, |p| &p.tracks);
    let height = if ui.available_height() < 400. {
        104.
    } else {
        152.
    };
    let (hero, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    widgets::gradient(ui.painter(), hero, 12., ACTIVE, SURFACE);
    ui.painter()
        .rect_stroke(hero, 12, Stroke::new(1., LINE), egui::StrokeKind::Inside);
    let cover = Rect::from_min_size(hero.min + vec2(16., 16.), vec2(height - 32., height - 32.));
    if let Some(collection) = &app.collection
        && let Some(url) = &collection.thumbnail
    {
        app.artwork.paint_url(
            ui,
            &collection.id,
            url,
            cover,
            if collection.kind == CollectionKind::Artist {
                60
            } else {
                8
            },
        );
    } else if let Some(track) = source.first() {
        app.artwork.paint(ui, track, cover, 8);
    } else {
        widgets::placeholder(ui, cover, 8, Icon::Library);
    }
    let text_x = cover.right() + 20.;
    let width = hero.right() - text_x - 16.;
    let kind = app
        .collection
        .as_ref()
        .map_or("PLAYLIST".to_owned(), |c| c.kind.label().to_uppercase());
    widgets::text(
        ui,
        Rect::from_min_size(pos2(text_x, hero.top() + 18.), vec2(width, 20.)),
        &kind,
        10.,
        MUTED,
    );
    widgets::text(
        ui,
        Rect::from_min_size(pos2(text_x, hero.top() + 40.), vec2(width, 34.)),
        title,
        if height < 120. { 24. } else { 30. },
        TEXT,
    );
    if height > 120. {
        widgets::text(
            ui,
            Rect::from_min_size(pos2(text_x, hero.top() + 82.), vec2(width, 22.)),
            subtitle,
            12.,
            MUTED,
        );
        widgets::text(
            ui,
            Rect::from_min_size(pos2(text_x, hero.top() + 112.), vec2(width, 20.)),
            &format!("{} songs", source.len()),
            11.,
            DIM,
        );
    }
    ui.add_space(16.);
    let saved = app
        .collection
        .as_ref()
        .is_some_and(|c| app.collection_saved(c));
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(!source.is_empty(), |ui| {
            if widgets::button(ui, "Play all", true).clicked() {
                actions.push(Action::PlayTracks(source.clone()));
            }
        });
        if ui
            .add_enabled(!source.is_empty(), egui::Button::new("Add all to queue"))
            .clicked()
        {
            actions.push(Action::QueueTracks(source.clone()));
        }
        if let Some(playlist) = playlist {
            if ui.button("Rename…").clicked() {
                app.playlist_editor = Some(PlaylistEditor::Rename {
                    id: playlist.id,
                    title: playlist.title.clone(),
                });
            }
            if ui.button("Delete…").clicked() {
                app.playlist_editor = Some(PlaylistEditor::Delete {
                    id: playlist.id,
                    title: playlist.title.clone(),
                });
            }
        } else if let Some(collection) = &app.collection
            && collection.kind != CollectionKind::Discography
        {
            let can_save = saved
                || collection.kind == CollectionKind::Artist
                || (!app.detail_loading
                    && app.detail_error.is_none()
                    && app.detail.continuation.is_none());
            let label = if collection.kind == CollectionKind::Playlist {
                "Import as editable playlist"
            } else if saved {
                "Remove from library"
            } else {
                "Save to library"
            };
            if ui
                .add_enabled(can_save, egui::Button::new(label))
                .on_hover_text("Load all tracks before saving a complete collection")
                .clicked()
            {
                actions.push(Action::SaveCollection);
            }
        }
    });
    ui.add_space(12.);
    if ui
        .add(
            egui::TextEdit::singleline(&mut app.filter)
                .hint_text("Filter tracks")
                .desired_width(220.),
        )
        .changed()
    {
        app.selected = None;
    }
    if let Some(error) = &app.detail_error {
        ui.label(RichText::new(error).color(ERROR));
        if ui.button("Try again").clicked() {
            actions.push(Action::RetryCollection);
        }
    }
    if app.detail_loading {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Loading collection…");
        });
    }
    ui.add_space(12.);
    let filter = app.filter.trim().to_lowercase();
    let indices: Option<Vec<usize>> = (!filter.is_empty()).then(|| {
        source
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.title.to_lowercase().contains(&filter)
                    || t.artist.to_lowercase().contains(&filter)
            })
            .map(|(i, _)| i)
            .collect()
    });
    let track_count = indices.as_ref().map_or(source.len(), Vec::len);
    let collections = &app.detail.collections;
    let count = track_count + collections.len();
    if count == 0 && !app.detail_loading && app.detail_error.is_none() {
        ui.label("No tracks here yet. Add songs from search, your library, or the queue.");
    }
    if app.detail.continuation.is_some()
        && ui
            .add_enabled(!app.detail_loading, egui::Button::new("Load more tracks"))
            .clicked()
    {
        actions.push(Action::CollectionMore);
    }
    let id = (
        "collection_tracks",
        app.playlist_id,
        app.collection.as_ref().map(|c| &c.id),
    );
    let height =
        (ui.available_height() - if app.playlist_id.is_some() { 52. } else { 0. }).max(32.);
    let row = app.selected.and_then(|selected| {
        indices.as_ref().map_or(Some(selected), |indices| {
            indices.iter().position(|i| *i == selected)
        })
    });
    let scroll = reveal(
        ui,
        egui::ScrollArea::vertical()
            .max_height(height)
            .min_scrolled_height(32.)
            .id_salt(id)
            .auto_shrink([false, false]),
        id,
        row,
        &mut app.reveal_selection,
        height,
    );
    scroll.show_rows(ui, 64., count, |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.;
        auto_scroll(ui);
        for index in range {
            if index >= track_count {
                collection_row(
                    ui,
                    &mut app.artwork,
                    &collections[index - track_count],
                    actions,
                );
            } else {
                let i = indices.as_ref().map_or(index, |indices| indices[index]);
                let track = &source[i];
                widgets::track_row(
                    ui,
                    &mut app.artwork,
                    Row {
                        track,
                        index: i,
                        current: app.current.as_ref().is_some_and(|t| t.id == track.id),
                        playing: app.playback == Playback::Playing,
                        liked: app.favorites.iter().any(|t| t.id == track.id),
                        selected: app.selected == Some(i),
                        queue: false,
                        collection: true,
                        radio: false,
                        queue_range: 0..source.len(),
                        saved: app.saved_tracks.iter().any(|t| t.id == track.id),
                        queue_revision: app.queue_revision,
                        playlist: app.playlist_id.map(|id| (id, app.playlist_revision)),
                    },
                    actions,
                );
            }
        }
    });
    if let Some(id) = app.playlist_id {
        drop_zone(
            ui,
            "Drop a song here to add to playlist",
            |drag| Action::PlaylistDrop(id, drag, source.len()),
            actions,
        );
    }
}

pub fn editor(app: &mut MusicApp, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(mut editor) = app.playlist_editor.take() else {
        return;
    };
    let mut close = false;
    let mut replacement = None;
    let response = egui::Modal::new(egui::Id::new("playlist_editor")).show(ctx, |ui| {
        ui.set_width(360.);
        match &mut editor {
            PlaylistEditor::Create { title, .. } | PlaylistEditor::Rename { title, .. } => {
                ui.heading("Playlist name");
                let field = ui.add(
                    egui::TextEdit::singleline(title)
                        .hint_text("Name your playlist")
                        .char_limit(200),
                );
                if !field.has_focus() && !ctx.memory(|m| m.focused().is_some()) {
                    field.request_focus();
                }
                let submit = ui
                    .add_enabled_ui(!title.trim().is_empty(), |ui| {
                        widgets::button(ui, "Save", true).clicked()
                    })
                    .inner
                    || (!title.trim().is_empty() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if submit {
                    actions.push(match &editor {
                        PlaylistEditor::Create { title, tracks } => {
                            Action::CreatePlaylist(title.clone(), tracks.clone())
                        }
                        PlaylistEditor::Rename { id, title } => {
                            Action::RenamePlaylist(*id, title.clone())
                        }
                        _ => unreachable!(),
                    });
                    close = true;
                }
            }
            PlaylistEditor::Delete { id, title } => {
                ui.heading("Delete playlist?");
                ui.label(title.as_str());
                if ui.button("Delete playlist").clicked() {
                    actions.push(Action::DeletePlaylist(*id));
                    close = true;
                }
            }
            PlaylistEditor::Add(track) => {
                ui.heading("Add to playlist");
                ui.label(&track.title);
                egui::ScrollArea::vertical()
                    .max_height(300.)
                    .show(ui, |ui| {
                        for playlist in &app.playlists {
                            if ui.button(&playlist.title).clicked() {
                                actions.push(Action::PlaylistAdd(
                                    playlist.id,
                                    track.clone(),
                                    playlist.tracks.len(),
                                ));
                                close = true;
                            }
                        }
                    });
                if ui.button("Create new playlist…").clicked() {
                    replacement = Some(PlaylistEditor::Create {
                        title: String::new(),
                        tracks: vec![track.clone()],
                    });
                }
            }
        }
        if ui.button("Cancel").clicked() {
            close = true;
        }
    });
    if let Some(editor) = replacement {
        app.playlist_editor = Some(editor);
    } else if !close && !response.should_close() {
        app.playlist_editor = Some(editor);
    }
}

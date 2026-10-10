use super::{
    icons::{self, Icon},
    theme::*,
    widgets::{self, Row},
};
use crate::app::{Action, LibraryTab, MusicApp, Playback, View};
use eframe::egui::{self, Align2, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

pub fn sidebar(app: &MusicApp, ui: &mut Ui, compact: bool, actions: &mut Vec<Action>) {
    ui.add_space(12.);
    let (brand, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.), Sense::hover());
    let logo = Rect::from_min_size(brand.min, vec2(36., 36.));
    ui.painter().rect_filled(logo, 12, ACCENT);
    // A compact waveform is Riff's native mark.
    for (i, h) in [9., 18., 24., 14.].iter().enumerate() {
        let x = logo.left() + 10. + i as f32 * 5.;
        ui.painter().line_segment(
            [
                pos2(x, logo.center().y - h / 2.),
                pos2(x, logo.center().y + h / 2.),
            ],
            Stroke::new(2.5, SIDEBAR),
        );
    }
    if !compact {
        ui.painter().text(
            pos2(brand.left() + 48., brand.center().y - 3.),
            Align2::LEFT_CENTER,
            "riff",
            FontId::proportional(30.),
            TEXT,
        );
    }
    ui.add_space(32.);
    for (view, icon, label) in [
        (View::Home, Icon::Home, "Home"),
        (View::Search, Icon::Search, "Search"),
        (View::Library, Icon::Library, "Library"),
        (View::Favorites, Icon::Heart, "Favorites"),
        (View::Albums, Icon::Music, "Albums"),
        (View::Artists, Icon::Music, "Artists"),
        (View::Playlists, Icon::Library, "Playlists"),
    ] {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 48.), Sense::hover());
        let response = ui.interact(rect, ui.id().with(view), Sense::click());
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::SelectableLabel,
                true,
                app.view == view,
                label,
            )
        });
        let selected = app.view == view;
        if selected || response.hovered() || response.has_focus() {
            ui.painter()
                .rect_filled(rect, 10, if selected { ACTIVE } else { SURFACE });
        }
        let color = if selected || response.hovered() {
            TEXT
        } else {
            MUTED
        };
        icons::paint(
            ui.painter(),
            icon,
            Rect::from_center_size(pos2(rect.left() + 24., rect.center().y), vec2(20., 20.)),
            color,
        );
        if !compact {
            widgets::text(
                ui,
                Rect::from_min_max(pos2(rect.left() + 48., rect.top()), rect.max),
                label,
                14.,
                color,
            );
        }
        if response.clicked() {
            actions.push(Action::Navigate(view));
        }
        if view == View::Playlists
            && let Some(drag) = response.dnd_release_payload::<crate::app::TrackDrag>()
        {
            actions.push(Action::AddToPlaylist(drag.track.clone()));
        }
        response.on_hover_text(if view == View::Search {
            "Search · Ctrl/Cmd+K"
        } else {
            label
        });
    }
    ui.add_space(24.);
    if !compact && ui.available_height() >= 216. {
        ui.painter().hline(
            ui.max_rect().x_range(),
            ui.cursor().top(),
            Stroke::new(1., LINE),
        );
        ui.add_space(24.);
        widgets::caption(ui, "YOUR COLLECTION");
        ui.add_space(12.);
        collection_shortcut(
            ui,
            "favorites_shortcut",
            Icon::Heart,
            "Favorite songs",
            &format!("{} saved", app.favorites.len()),
            actions,
            Action::Navigate(View::Favorites),
        );
        collection_shortcut(
            ui,
            "queue_shortcut",
            Icon::Queue,
            "Your queue",
            &format!("{} up next", app.queue_len()),
            actions,
            Action::ToggleQueue,
        );
    } else if !compact {
        collection_shortcut(
            ui,
            "queue_shortcut",
            Icon::Queue,
            "Your queue",
            &format!("{} up next", app.queue_len()),
            actions,
            Action::ToggleQueue,
        );
    } else {
        let (r, _) = ui.allocate_exact_size(vec2(48., 48.), Sense::hover());
        if widgets::icon_button(
            ui,
            "queue_nav",
            r,
            Icon::Queue,
            "Open queue · Ctrl/Cmd+J",
            app.queue_open,
            true,
            false,
        )
        .clicked()
        {
            actions.push(Action::ToggleQueue);
        }
    }
    if ui.available_height() > 100. {
        let r = ui.max_rect();
        if !compact {
            widgets::text(
                ui,
                Rect::from_min_size(
                    pos2(r.left() + 8., r.bottom() - 56.),
                    vec2(r.width() - 16., 24.),
                ),
                "Good music. Your way.",
                12.,
                MUTED,
            );
            widgets::text(
                ui,
                Rect::from_min_size(
                    pos2(r.left() + 8., r.bottom() - 32.),
                    vec2(r.width() - 16., 24.),
                ),
                "SPACE  Play / pause",
                11.,
                DIM,
            );
        }
    }
}
fn collection_shortcut(
    ui: &mut Ui,
    id: &str,
    icon: Icon,
    title: &str,
    subtitle: &str,
    actions: &mut Vec<Action>,
    action: Action,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.), Sense::hover());
    let response = ui.interact(rect, ui.id().with(id), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 8, SURFACE);
    }
    let cover = Rect::from_min_size(pos2(rect.left() + 4., rect.top() + 12.), vec2(40., 40.));
    ui.painter().rect_filled(cover, 8, ACTIVE);
    icons::paint(ui.painter(), icon, cover.shrink(11.), ACCENT);
    widgets::text(
        ui,
        Rect::from_min_size(
            pos2(cover.right() + 12., rect.top() + 10.),
            vec2(rect.width() - 60., 24.),
        ),
        title,
        12.,
        TEXT,
    );
    widgets::text(
        ui,
        Rect::from_min_size(
            pos2(cover.right() + 12., rect.top() + 32.),
            vec2(rect.width() - 60., 24.),
        ),
        subtitle,
        11.,
        MUTED,
    );
    if response.clicked() {
        actions.push(action);
    }
}

pub fn topbar(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    let width = ui.available_width();
    ui.horizontal(|ui| {
        let search_width = (width - 152.).min(608.);
        egui::Frame::new()
            .fill(SURFACE)
            .corner_radius(24)
            .inner_margin(egui::Margin::symmetric(16, 8))
            .show(ui, |ui| {
                ui.set_width(search_width - 32.);
                ui.spacing_mut().interact_size.y = 32.;
                ui.horizontal(|ui| {
                    let (rect, _) = ui.allocate_exact_size(vec2(20., 24.), Sense::hover());
                    icons::paint(
                        ui.painter(),
                        Icon::Search,
                        rect.shrink2(vec2(0., 2.)),
                        MUTED,
                    );
                    if app.focus_search {
                        ui.memory_mut(|m| m.request_focus(egui::Id::new("global_search")));
                        app.focus_search = false;
                    }
                    let response = ui.add_sized(
                        [search_width - 148., 32.],
                        egui::TextEdit::singleline(&mut app.query)
                            .id(egui::Id::new("global_search"))
                            .hint_text(
                                RichText::new("Search songs, artists, albums, playlists")
                                    .color(MUTED),
                            )
                            .text_color(TEXT)
                            .frame(egui::Frame::NONE)
                            .margin(egui::Margin::ZERO)
                            .vertical_align(egui::Align::Center)
                            .font(FontId::proportional(14.)),
                    );
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        actions.push(Action::Search);
                    }
                    let (r, _) = ui.allocate_exact_size(vec2(32., 32.), Sense::hover());
                    if !app.query.is_empty()
                        && widgets::icon_button(
                            ui,
                            "clear_search",
                            r,
                            Icon::Close,
                            "Clear search",
                            false,
                            true,
                            false,
                        )
                        .clicked()
                    {
                        app.query.clear();
                        app.focus_search = true;
                    }
                    let (submit, _) = ui.allocate_exact_size(vec2(32., 32.), Sense::hover());
                    if widgets::icon_button(
                        ui,
                        "submit_search",
                        submit,
                        Icon::Arrow,
                        "Search · Enter",
                        false,
                        !app.query.trim().is_empty(),
                        false,
                    )
                    .clicked()
                    {
                        actions.push(Action::Search);
                    }
                });
            });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (r, _) = ui.allocate_exact_size(vec2(40., 40.), Sense::hover());
            let drop = ui.interact(r, ui.id().with("queue_drop"), Sense::hover());
            if let Some(drag) = drop.dnd_release_payload::<crate::app::TrackDrag>() {
                actions.push(Action::QueueDrop((*drag).clone(), app.queue_len()));
                if !app.queue_open {
                    actions.push(Action::ToggleQueue);
                }
            }
            if widgets::icon_button(
                ui,
                "top_queue",
                r,
                Icon::Queue,
                "Queue · Ctrl/Cmd+J",
                app.queue_open,
                true,
                false,
            )
            .clicked()
            {
                actions.push(Action::ToggleQueue);
            }
            if ui
                .button(if app.account.library.is_some() {
                    "Account"
                } else {
                    "Sign in"
                })
                .on_hover_text("Google / YouTube account")
                .clicked()
            {
                actions.push(Action::Navigate(View::Account));
            }
        });
    });
}

pub fn content(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    match app.view {
        View::Home => home(app, ui, actions),
        View::Search => super::collections::search(app, ui, actions),
        View::Library | View::Favorites => library(app, ui, actions),
        View::Albums | View::Artists | View::Playlists => {
            super::collections::library(app, ui, actions)
        }
        View::Collection => super::collections::detail(app, ui, actions),
        View::Account => super::account::show(app, ui, actions),
    }
}
fn home(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    let compact = ui.available_height() < 500.;
    egui::ScrollArea::vertical()
        .id_salt("home_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if compact {
                ui.label(RichText::new("Find your next favorite").font(bold(26.)));
            } else {
                widgets::heading(
                    ui,
                    "Find your next favorite",
                    "A little familiar. Something new.",
                );
            }
            ui.add_space(12.);
            if !compact {
                home_hero(app, ui, actions, false);
                ui.add_space(16.);
            }
            if app.recommendations.seed.is_some() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Recommended for you").font(bold(20.)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_enabled_ui(!app.recommendations.searching, |ui| {
                            if widgets::button(ui, "Refresh", false).clicked() {
                                actions.push(Action::RefreshRecommendations);
                            }
                        });
                    });
                });
                if !compact {
                    ui.label(
                        RichText::new(if app.account.library.is_some() {
                            "Related songs inspired by your listening and YouTube likes."
                        } else {
                            "Related songs from YouTube, inspired by your listening."
                        })
                        .color(MUTED),
                    );
                }
                ui.add_space(8.);
                if app.recommendations.searching {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Finding songs for you…").color(MUTED));
                    });
                    ui.add_space(12.);
                    let count = ((ui.available_width() + 16.) / 164.).floor().max(1.) as usize;
                    ui.horizontal(|ui| {
                        for _ in 0..count {
                            let (r, _) = ui.allocate_exact_size(vec2(148., 210.), Sense::hover());
                            ui.painter().rect_filled(
                                Rect::from_min_size(r.min, vec2(148., 148.)),
                                10,
                                SURFACE,
                            );
                            for (y, width) in [(164., 124.), (190., 84.)] {
                                ui.painter().rect_filled(
                                    Rect::from_min_size(r.min + vec2(0., y), vec2(width, 10.)),
                                    4,
                                    SURFACE,
                                );
                            }
                        }
                    });
                } else if let Some(error) = &app.recommendations.error {
                    egui::Frame::new()
                        .fill(SURFACE)
                        .corner_radius(12)
                        .inner_margin(20)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(
                                RichText::new("Couldn't load your recommendations").font(bold(16.)),
                            );
                            ui.add(egui::Label::new(RichText::new(error).color(MUTED)).wrap());
                            ui.add_space(8.);
                            if widgets::button(ui, "Try again", true).clicked() {
                                actions.push(Action::RefreshRecommendations);
                            }
                        });
                } else if app.recommendations.tracks.is_empty() {
                    ui.label(
                        RichText::new(
                            "No new songs in this mix yet. Play a different song, then refresh.",
                        )
                        .color(MUTED),
                    );
                    ui.add_space(8.);
                    if widgets::button(ui, "Explore music", false).clicked() {
                        actions.push(Action::FocusSearch);
                    }
                } else {
                    track_shelf(app, ui, actions, true, compact);
                }
                ui.add_space(24.);
            } else {
                ui.label(RichText::new("Find your starting point").font(bold(20.)));
                ui.label(
                    RichText::new("Play a song or save a favorite to get related recommendations.")
                        .color(MUTED),
                );
                ui.add_space(12.);
                ui.horizontal_wrapped(|ui| {
                    for (label, query) in [
                        ("Chill & focus", "chill lofi music"),
                        ("Indie discoveries", "indie music songs"),
                        ("Feel-good pop", "feel good pop songs"),
                        ("Jazz essentials", "jazz classics music"),
                    ] {
                        if widgets::button(ui, label, false).clicked() {
                            actions.push(Action::Discover(query));
                        }
                    }
                });
                ui.add_space(24.);
            }
            if compact {
                home_hero(app, ui, actions, true);
                ui.add_space(16.);
            }
            ui.label(RichText::new("Your music, a tap away").font(bold(20.)));
            ui.add_space(12.);
            let w = (ui.available_width() - 8.) / 2.;
            ui.horizontal(|ui| {
                quick_card(
                    ui,
                    w,
                    Icon::Heart,
                    "Favorite songs",
                    &format!("{} songs, all yours", app.favorites.len()),
                    Action::Navigate(View::Favorites),
                    actions,
                );
                quick_card(
                    ui,
                    w,
                    Icon::Queue,
                    "Your queue",
                    &format!("{} tracks up next", app.queue_len()),
                    Action::ToggleQueue,
                    actions,
                );
            });
            if !app.recent.is_empty() {
                ui.add_space(24.);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Recently played").font(bold(20.)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "View library", false).clicked() {
                            actions.push(Action::Navigate(View::Library));
                        }
                    });
                });
                ui.add_space(12.);
                track_shelf(app, ui, actions, false, compact);
            }
            ui.add_space(24.);
        });
}

fn home_hero(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>, compact: bool) {
    let (hero, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), if compact { 104. } else { 132. }),
        Sense::hover(),
    );
    ui.painter().rect_filled(hero, 16, SURFACE);
    let wide = hero.width() > 560.;
    let text_width = hero.width() - if wide { 156. } else { 48. };
    let seed = app.recommendations.seed.as_ref();
    widgets::text(
        ui,
        Rect::from_min_size(hero.min + vec2(24., 8.), vec2(text_width, 20.)),
        if seed.is_some() {
            "INSPIRED BY YOUR MUSIC"
        } else {
            "A SPACE FOR YOUR SOUND"
        },
        10.,
        MUTED,
    );
    widgets::text(
        ui,
        Rect::from_min_size(hero.min + vec2(24., 28.), vec2(text_width, 28.)),
        seed.map_or("Good music starts with you.", |t| t.title.as_str()),
        22.,
        TEXT,
    );
    if !compact {
        widgets::text(
            ui,
            Rect::from_min_size(hero.min + vec2(24., 54.), vec2(text_width, 24.)),
            seed.map_or("Find a song you love. We'll take it from there.", |t| {
                t.artist.as_str()
            }),
            13.,
            MUTED,
        );
    }
    if wide {
        let size = if compact { 80. } else { 108. };
        let cover = Rect::from_min_size(
            pos2(hero.right() - size - 16., hero.top() + 12.),
            vec2(size, size),
        );
        if let Some(track) = seed {
            app.artwork.paint(ui, track, cover, 12);
        } else {
            let p = ui.painter().with_clip_rect(hero);
            let center = cover.center();
            p.circle_filled(center, size * 0.5, HOVER);
            p.circle_filled(center, size * 0.44, SIDEBAR);
            for radius in [0.22, 0.27, 0.32, 0.37, 0.42] {
                p.circle_stroke(center, size * radius, Stroke::new(1., LINE));
            }
            p.circle_filled(center, 20., ACCENT);
            p.circle_filled(center, 4., SIDEBAR);
        }
    }
    let cta = Rect::from_min_size(
        hero.min + vec2(24., if compact { 64. } else { 88. }),
        vec2(164., 32.),
    );
    let response = ui.interact(cta, ui.id().with("hero_action"), Sense::click());
    let label = if seed.is_some() {
        "Start song radio"
    } else {
        "Explore music"
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    ui.painter().rect_filled(
        cta,
        16,
        if response.hovered() || response.has_focus() {
            TEXT
        } else {
            ACCENT
        },
    );
    icons::paint(
        ui.painter(),
        if seed.is_some() {
            Icon::Play
        } else {
            Icon::Search
        },
        Rect::from_min_size(cta.min + vec2(12., 8.), vec2(16., 16.)),
        SIDEBAR,
    );
    widgets::text(
        ui,
        Rect::from_min_size(cta.min + vec2(36., 0.), vec2(120., 32.)),
        label,
        12.,
        SIDEBAR,
    );
    if response.clicked() {
        actions.push(seed.map_or(Action::FocusSearch, |t| Action::StartRadio(t.clone())));
    }
}

fn track_shelf(
    app: &mut MusicApp,
    ui: &mut Ui,
    actions: &mut Vec<Action>,
    recommended: bool,
    compact: bool,
) {
    let id = if recommended {
        "recommendation_shelf"
    } else {
        "recent_shelf"
    };
    let tracks = if recommended {
        &app.recommendations.tracks
    } else {
        &app.recent
    };
    let selectable = recommended || app.recommendations.tracks.is_empty();
    let target = if compact { 128. } else { 156. };
    let count = ((ui.available_width() + 16.) / (target + 16.))
        .floor()
        .max(1.) as usize;
    let width =
        ((ui.available_width() - 16. * (count - 1) as f32) / count as f32).clamp(112., target);
    egui::ScrollArea::horizontal()
        .id_salt(id)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 16.;
                for (i, track) in tracks.iter().take(12).enumerate() {
                    ui.push_id((id, &track.id), |ui| {
                        let (r, _) =
                            ui.allocate_exact_size(vec2(width, width + 100.), Sense::hover());
                        let selected = selectable && app.selected == Some(i);
                        let play_rect = Rect::from_min_size(r.min, vec2(width, width + 64.));
                        let response =
                            ui.interact(play_rect, ui.id().with("play"), Sense::click_and_drag());
                        if response.drag_started() {
                            response.dnd_set_drag_payload(crate::app::TrackDrag {
                                track: track.clone(),
                                origin: crate::app::DragOrigin::Track,
                            });
                        }
                        response.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::SelectableLabel,
                                true,
                                selected,
                                format!("Play {} · {}", track.title, track.artist),
                            )
                        });
                        if selected {
                            ui.ctx().data_mut(|d| {
                                d.insert_temp(egui::Id::new("selected_row_focus"), response.id)
                            });
                        }
                        if selected || response.hovered() || response.has_focus() {
                            ui.painter().rect_filled(r, 10, SURFACE);
                        }
                        if selected && app.reveal_selection {
                            response.scroll_to_me(Some(egui::Align::Center));
                            app.reveal_selection = false;
                        }
                        let cover = Rect::from_min_size(r.min, vec2(width, width));
                        app.artwork.paint(ui, track, cover, 10);
                        if response.hovered() || response.has_focus() || selected {
                            let b = Rect::from_center_size(
                                cover.right_bottom() - vec2(26., 26.),
                                vec2(40., 40.),
                            );
                            ui.painter().circle_filled(b.center(), 20., ACCENT);
                            icons::paint(ui.painter(), Icon::Play, b.shrink(11.), TEXT);
                        }
                        widgets::text(
                            ui,
                            Rect::from_min_size(
                                pos2(r.left(), cover.bottom() + 10.),
                                vec2(width, 24.),
                            ),
                            &track.title,
                            13.,
                            if app.current.as_ref().is_some_and(|t| t.id == track.id) {
                                ACCENT
                            } else {
                                TEXT
                            },
                        );
                        widgets::text(
                            ui,
                            Rect::from_min_size(
                                pos2(r.left(), cover.bottom() + 34.),
                                vec2(width, 20.),
                            ),
                            &track.artist,
                            12.,
                            MUTED,
                        );
                        if response.clicked() {
                            if selectable {
                                actions.push(Action::Select(i));
                            }
                            actions.push(Action::Play(track.clone()));
                        }
                        response.on_hover_text(format!("{}\n{}", track.title, track.artist));
                        let liked = app.favorites.iter().any(|t| t.id == track.id);
                        let favorite = Rect::from_min_size(
                            pos2(r.left(), cover.bottom() + 62.),
                            vec2(32., 32.),
                        );
                        if widgets::icon_button(
                            ui,
                            "favorite",
                            favorite,
                            if liked { Icon::Check } else { Icon::Heart },
                            if liked {
                                "Remove from favorites"
                            } else {
                                "Save to favorites"
                            },
                            liked,
                            true,
                            false,
                        )
                        .clicked()
                        {
                            actions.push(Action::Favorite(track.clone()));
                        }
                        let queue = favorite.translate(vec2(40., 0.));
                        if widgets::icon_button(
                            ui,
                            "enqueue",
                            queue,
                            Icon::Queue,
                            "Add to queue",
                            false,
                            true,
                            false,
                        )
                        .clicked()
                        {
                            actions.push(Action::Enqueue(track.clone(), false));
                        }
                    });
                }
            });
        });
}
fn quick_card(
    ui: &mut Ui,
    width: f32,
    icon: Icon,
    title: &str,
    subtitle: &str,
    action: Action,
    actions: &mut Vec<Action>,
) {
    let (r, _) = ui.allocate_exact_size(vec2(width, 88.), Sense::hover());
    let response = ui.interact(r, ui.id().with(title), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    ui.painter()
        .rect_filled(r, 12, if response.hovered() { HOVER } else { SURFACE });
    let art = Rect::from_min_size(pos2(r.left() + 16., r.top() + 16.), vec2(56., 56.));
    ui.painter().rect_filled(art, 10, ACTIVE);
    icons::paint(ui.painter(), icon, art.shrink(17.), ACCENT);
    widgets::text(
        ui,
        Rect::from_min_size(
            pos2(art.right() + 16., r.top() + 18.),
            vec2(width - 104., 24.),
        ),
        title,
        14.,
        TEXT,
    );
    widgets::text(
        ui,
        Rect::from_min_size(
            pos2(art.right() + 16., r.top() + 46.),
            vec2(width - 104., 20.),
        ),
        subtitle,
        12.,
        MUTED,
    );
    if response.clicked() {
        actions.push(action);
    }
}
fn library(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    let favorites = app.view == View::Favorites;
    if favorites {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(80., 80.), Sense::hover());
            ui.painter().rect_filled(r, 16, ACTIVE);
            icons::paint(ui.painter(), Icon::Heart, r.shrink(25.), ACCENT);
            ui.vertical(|ui| {
                widgets::caption(ui, "YOUR COLLECTION");
                widgets::heading(
                    ui,
                    "Favorite songs",
                    &format!("{} songs you want to come back to", app.favorites.len()),
                );
            });
        });
    } else {
        widgets::heading(
            ui,
            "Your library",
            "A home for your favorites and recently played music.",
        );
    }
    ui.add_space(24.);
    ui.horizontal(|ui| {
        if favorites {
            ui.add_enabled_ui(!app.favorites.is_empty(), |ui| {
                if widgets::button(ui, "Play favorites", true).clicked() {
                    actions.push(Action::PlayFavorites);
                }
            });
        } else {
            for (tab, label) in [
                (LibraryTab::All, "All music"),
                (LibraryTab::Saved, "Saved"),
                (LibraryTab::Favorites, "Favorites"),
                (LibraryTab::Recent, "Recent"),
            ] {
                if ui
                    .add(
                        egui::Button::new(RichText::new(label).size(12.).color(
                            if tab == app.library_tab {
                                SIDEBAR
                            } else {
                                MUTED
                            },
                        ))
                        .fill(if tab == app.library_tab {
                            ACCENT
                        } else {
                            SURFACE
                        })
                        .corner_radius(18),
                    )
                    .clicked()
                {
                    actions.push(Action::LibraryTab(tab));
                }
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let width = if ui.available_width() > 260. {
                200.
            } else {
                ui.available_width().max(72.)
            };
            let response = ui.add_sized(
                [width, 36.],
                egui::TextEdit::singleline(&mut app.filter)
                    .hint_text(RichText::new("Filter your music").color(MUTED))
                    .text_color(TEXT)
                    .frame(
                        egui::Frame::new()
                            .fill(SURFACE)
                            .corner_radius(18)
                            .inner_margin(egui::Margin::symmetric(12, 8)),
                    )
                    .margin(egui::Margin::symmetric(12, 8)),
            );
            if response.changed() {
                actions.push(Action::RefreshLibrary);
            }
        });
    });
    ui.add_space(24.);
    if app.library.is_empty() {
        let (title, description) = if !app.filter.is_empty() {
            ("No matching songs", "Try a different title or artist.")
        } else if favorites {
            (
                "Keep the songs you love",
                "Save a song with the heart beside it. Your favorites stay here between visits.",
            )
        } else {
            (
                "Your collection starts here",
                "Play something or save a favorite. Your music will appear here.",
            )
        };
        if widgets::empty(
            ui,
            if favorites {
                Icon::Heart
            } else {
                Icon::Library
            },
            title,
            description,
            if app.filter.is_empty() {
                "Find some music"
            } else {
                ""
            },
        ) {
            actions.push(Action::FocusSearch);
        }
    } else {
        widgets::caption(ui, &format!("{} SONGS", app.library.len()));
        ui.add_space(8.);
        tracks(
            app,
            ui,
            actions,
            if favorites {
                "favorites_list"
            } else {
                "library_list"
            },
        );
    }
}
fn tracks(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>, id: &str) {
    let source = if app.view == View::Search {
        &app.results
    } else {
        &app.library
    };
    ui.spacing_mut().item_spacing.y = 0.;
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false]);
    if app.reveal_selection {
        app.reveal_selection = false;
        if let Some(i) = app.selected {
            let offset = egui::scroll_area::State::load(ui.ctx(), ui.make_persistent_id(id))
                .map_or(0., |s| s.offset.y);
            let y = i as f32 * 64.;
            let height = ui.available_height();
            scroll = scroll.vertical_scroll_offset(if y < offset {
                y
            } else if y + 64. > offset + height {
                y + 64. - height
            } else {
                offset
            });
        }
    }
    scroll.show_rows(ui, 64., source.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.;
        for i in range {
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
                    radio: false,
                    queue_range: 0..0,
                    saved: app.saved_tracks.iter().any(|t| t.id == track.id),
                    queue_revision: app.queue_revision,
                    playlist: None,
                },
                actions,
            );
        }
    });
}

use super::{
    carousel::Carousel,
    icons::{self, Icon},
    theme::*,
    widgets::{self, Row},
};
use crate::app::{Action, LibraryTab, MusicApp, Playback, View};
use eframe::egui::{self, Align2, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

pub fn sidebar(app: &mut MusicApp, ui: &mut Ui, compact: bool, actions: &mut Vec<Action>) {
    ui.add_space(if cfg!(target_os = "macos") { 32. } else { 12. });
    let (brand, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.), Sense::hover());
    let logo = Rect::from_min_size(brand.min + vec2(8., 2.), vec2(34., 34.));
    widgets::glow(ui.painter(), logo.center(), 14.);
    ui.painter().rect_filled(logo, 10, ACCENT);
    for (i, h) in [9., 18., 24., 14.].iter().enumerate() {
        let x = logo.left() + 9. + i as f32 * 5.;
        ui.painter().line_segment(
            [
                pos2(x, logo.center().y - h / 2.),
                pos2(x, logo.center().y + h / 2.),
            ],
            Stroke::new(2.5, TEXT),
        );
    }
    if !compact {
        ui.painter().text(
            pos2(brand.left() + 54., brand.center().y),
            Align2::LEFT_CENTER,
            "Riff",
            bold(28.),
            TEXT,
        );
    }
    ui.add_space(24.);
    let show_footer = !compact && ui.available_height() > 670.;
    let height = ui.available_height() - if show_footer { 176. } else { 0. };
    egui::ScrollArea::vertical()
        .id_salt("sidebar_scroll")
        .max_height(height)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.;
            for (view, icon, label) in [
                (View::Home, Icon::Home, "Home"),
                (View::Search, Icon::Search, "Explore"),
                (View::Library, Icon::Library, "Library"),
            ] {
                navigation(
                    ui,
                    app.view == view,
                    compact,
                    icon,
                    label,
                    actions,
                    Action::Navigate(view),
                );
            }
            ui.add_space(20.);
            if !compact {
                widgets::caption(ui, "YOUR COLLECTION");
                ui.add_space(10.);
            }
            for (view, icon, label) in [
                (View::Favorites, Icon::Heart, "Liked Music"),
                (View::Albums, Icon::Music, "Albums"),
                (View::Artists, Icon::Radio, "Artists"),
                (View::Playlists, Icon::Library, "Playlists"),
            ] {
                navigation(
                    ui,
                    app.view == view,
                    compact,
                    icon,
                    label,
                    actions,
                    Action::Navigate(view),
                );
            }
            navigation(
                ui,
                app.queue_open,
                compact,
                Icon::Queue,
                "Your queue",
                actions,
                Action::ToggleQueue,
            );
            ui.add_space(14.);
            ui.separator();
            ui.add_space(12.);
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.), Sense::hover());
            let response = ui.interact(r, ui.id().with("new_playlist"), Sense::click());
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New playlist")
            });
            if response.hovered() || response.has_focus() {
                ui.painter().rect_filled(r, 10, SURFACE);
            }
            let center = pos2(r.left() + 24., r.center().y);
            ui.painter().line_segment(
                [center - vec2(8., 0.), center + vec2(8., 0.)],
                Stroke::new(1.5, TEXT),
            );
            ui.painter().line_segment(
                [center - vec2(0., 8.), center + vec2(0., 8.)],
                Stroke::new(1.5, TEXT),
            );
            if !compact {
                widgets::text(
                    ui,
                    Rect::from_min_max(r.min + vec2(48., 0.), r.max),
                    "New Playlist",
                    13.,
                    TEXT,
                );
            }
            if response.clicked() {
                app.playlist_editor = Some(crate::app::PlaylistEditor::Create {
                    title: String::new(),
                    tracks: Vec::new(),
                });
            }
            response.on_hover_text("Create a playlist");
        });
    if show_footer {
        let r = Rect::from_min_size(
            pos2(ui.max_rect().left(), ui.max_rect().bottom() - 160.),
            vec2(ui.available_width(), 148.),
        );
        widgets::gradient(
            ui.painter(),
            r,
            12.,
            egui::Color32::from_rgb(44, 17, 40),
            SIDEBAR,
        );
        ui.painter()
            .rect_stroke(r, 12, Stroke::new(1., LINE), egui::StrokeKind::Inside);
        ui.painter().text(
            r.min + vec2(16., 20.),
            Align2::LEFT_TOP,
            "Music without limits",
            bold(15.),
            TEXT,
        );
        ui.painter().text(
            r.min + vec2(16., 49.),
            Align2::LEFT_TOP,
            "Your songs. Your rhythm.",
            FontId::proportional(11.),
            MUTED,
        );
        let cta = Rect::from_min_size(r.min + vec2(16., 88.), vec2(r.width() - 32., 36.));
        let response = ui.interact(cta, ui.id().with("sidebar_explore"), Sense::click());
        ui.painter().rect_filled(cta, 18, ACTIVE);
        ui.painter()
            .rect_stroke(cta, 18, Stroke::new(1., ACCENT), egui::StrokeKind::Inside);
        ui.painter().text(
            cta.center(),
            Align2::CENTER_CENTER,
            "Explore your sound",
            bold(11.),
            TEXT,
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Explore your sound")
        });
        if response.clicked() {
            actions.push(Action::FocusSearch);
        }
    }
}

fn navigation(
    ui: &mut Ui,
    selected: bool,
    compact: bool,
    icon: Icon,
    label: &str,
    actions: &mut Vec<Action>,
    action: Action,
) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.), Sense::hover());
    let response = ui.interact(rect, ui.id().with(label), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    if selected {
        widgets::gradient(ui.painter(), rect, 10., ACTIVE, SURFACE);
        ui.painter()
            .rect_stroke(rect, 10, Stroke::new(1., LINE), egui::StrokeKind::Inside);
    } else if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 10, SURFACE);
    }
    icons::paint(
        ui.painter(),
        icon,
        Rect::from_center_size(pos2(rect.left() + 24., rect.center().y), vec2(20., 20.)),
        if selected { ACCENT } else { TEXT },
    );
    if !compact {
        widgets::text(
            ui,
            Rect::from_min_max(rect.min + vec2(48., 0.), rect.max),
            label,
            13.,
            TEXT,
        );
    }
    if response.clicked() {
        actions.push(action);
    }
    if label == "Playlists"
        && let Some(drag) = response.dnd_release_payload::<crate::app::TrackDrag>()
    {
        actions.push(Action::AddToPlaylist(drag.track.clone()));
    }
    response.on_hover_text(label);
}

pub fn topbar(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.), Sense::hover());
    let search_width = (bar.width() * 0.5)
        .clamp(300., 620.)
        .min(bar.width() - 100.);
    let search = Rect::from_min_size(bar.min, vec2(search_width, 40.));
    let drag_rect = Rect::from_min_max(
        pos2(search.right() + 12., bar.top()),
        pos2(bar.right() - 96., bar.bottom()),
    );
    if drag_rect.is_positive() {
        let drag = ui.interact(
            drag_rect,
            ui.id().with("header_drag"),
            Sense::click_and_drag(),
        );
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
    }
    widgets::gradient(
        ui.painter(),
        search,
        20.,
        SURFACE,
        HOVER.gamma_multiply(0.8),
    );
    ui.painter().rect_stroke(
        search,
        20,
        Stroke::new(1., LINE.gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
    icons::paint(
        ui.painter(),
        Icon::Search,
        Rect::from_center_size(search.min + vec2(22., 20.), vec2(18., 18.)),
        TEXT,
    );
    if app.focus_search {
        ui.memory_mut(|m| m.request_focus(egui::Id::new("global_search")));
        app.focus_search = false;
    }
    let has_query = !app.query.is_empty();
    let input = Rect::from_min_max(
        search.min + vec2(44., 6.),
        search.max - vec2(if has_query { 72. } else { 16. }, 6.),
    );
    let response = ui.put(
        input,
        egui::TextEdit::singleline(&mut app.query)
            .id(egui::Id::new("global_search"))
            .hint_text(RichText::new("Search songs, artists, albums, playlists...").color(MUTED))
            .text_color(TEXT)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .vertical_align(egui::Align::Center)
            .font(FontId::proportional(13.)),
    );
    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
        actions.push(Action::Search);
    }
    if has_query {
        let clear = Rect::from_center_size(
            pos2(search.right() - 54., search.center().y),
            vec2(28., 28.),
        );
        if widgets::icon_button(
            ui,
            "clear_search",
            clear,
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
        let submit = clear.translate(vec2(30., 0.));
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
    }
    let avatar = Rect::from_center_size(pos2(bar.right() - 18., bar.center().y), vec2(36., 36.));
    let response = ui.interact(avatar, ui.id().with("account_avatar"), Sense::click());
    let library = app.account.library.as_ref();
    let has_photo = library
        .and_then(|library| library.picture.as_deref())
        .is_some_and(|url| app.artwork.paint_profile(ui, url, avatar));
    if !has_photo {
        let initial = library
            .map_or("R", |library| library.name.as_str())
            .chars()
            .next()
            .unwrap_or('R')
            .to_uppercase()
            .to_string();
        ui.painter().circle_filled(avatar.center(), 18., ACTIVE);
        ui.painter().text(
            avatar.center(),
            Align2::CENTER_CENTER,
            initial,
            bold(14.),
            TEXT,
        );
    }
    ui.painter().circle_stroke(
        avatar.center(),
        18.,
        Stroke::new(
            1.5,
            if app.view == View::Account || response.hovered() || response.has_focus() {
                ACCENT
            } else {
                TEXT.gamma_multiply(0.65)
            },
        ),
    );
    let label = if library.is_some() {
        "Your account"
    } else {
        "Sign in with Google"
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    if response.clicked() {
        actions.push(Action::Navigate(View::Account));
    }
    response.on_hover_text(label);

    let bell = avatar.translate(vec2(-52., 0.));
    let response = ui.interact(bell, ui.id().with("notifications"), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Notifications"));
    ui.painter().circle_filled(
        bell.center(),
        18.,
        if response.hovered() || response.has_focus() {
            HOVER
        } else {
            SURFACE
        },
    );
    icons::paint(
        ui.painter(),
        Icon::Bell,
        Rect::from_center_size(bell.center(), vec2(21., 21.)),
        TEXT,
    );
    let has_notice = app.notice.is_some()
        || app.global_error.is_some()
        || app.account.error.is_some()
        || app.playback_error.is_some();
    if has_notice {
        ui.painter()
            .circle_filled(bell.right_top() + vec2(-5., 6.), 3.5, ACCENT);
    }
    egui::Popup::from_toggle_button_response(&response).show(|ui| {
        ui.set_width(280.);
        ui.label(RichText::new("Notifications").font(bold(17.)));
        ui.add_space(8.);
        if let Some(notice) = &app.notice {
            ui.add(egui::Label::new(&notice.text).wrap());
            if ui.small_button("Dismiss notice").clicked() {
                actions.push(Action::DismissNotice);
            }
        }
        if let Some(error) = &app.global_error {
            ui.add(egui::Label::new(RichText::new(error).color(ERROR)).wrap());
            if ui.small_button("Dismiss error").clicked() {
                actions.push(Action::DismissError);
            }
        }
        if let Some(error) = &app.playback_error {
            ui.add(egui::Label::new(RichText::new(error).color(ERROR)).wrap());
            if ui.small_button("Retry playback").clicked() {
                actions.push(Action::TogglePlayback);
            }
        }
        if let Some(error) = &app.account.error {
            ui.add(egui::Label::new(RichText::new(error).color(ERROR)).wrap());
            if ui.small_button("Open account").clicked() {
                actions.push(Action::Navigate(View::Account));
            }
        }
        if !has_notice {
            ui.label(RichText::new("You're all caught up.").color(MUTED));
        }
    });
    response.on_hover_text("Notifications");
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
    let compact = ui.available_width() < 740. || ui.available_height() < 640.;
    egui::ScrollArea::vertical()
        .id_salt("home_scroll")
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            home_hero(app, ui, actions, compact);
            ui.add_space(16.);
            let discovery_width = ((ui.available_width() - 5. * 12.) / 6.).max(136.);
            let mut discovery = Carousel::new(ui, "discovery_shelf", 6, discovery_width);
            section_header(
                ui,
                "Your daily soundtrack",
                "",
                None,
                Some(&mut discovery),
                actions,
            );
            ui.add_space(8.);
            discovery_shelf(ui, actions, &discovery);
            ui.add_space(16.);
            let mut recommendations = Carousel::new(
                ui,
                "recommendation_shelf",
                app.recommendations.tracks.len().min(12),
                if compact { 144. } else { 160. },
            );
            let recommendations_ready = !app.recommendations.tracks.is_empty();
            section_header(
                ui,
                "Made for you",
                "Personalized discoveries inspired by your taste",
                app.recommendations.seed.as_ref().map(|_| {
                    (
                        if app.recommendations.searching {
                            "Refreshing…"
                        } else {
                            "Refresh"
                        },
                        Action::RefreshRecommendations,
                        !app.recommendations.searching,
                    )
                }),
                recommendations_ready.then_some(&mut recommendations),
                actions,
            );
            ui.add_space(8.);
            if recommendations_ready {
                if let Some(error) = &app.recommendations.error {
                    ui.label(
                        RichText::new("Couldn't refresh. Showing your previous mix.")
                            .size(12.)
                            .color(MUTED),
                    )
                    .on_hover_text(error);
                    ui.add_space(4.);
                }
                track_shelf(app, ui, actions, true, &recommendations);
            } else if app.recommendations.searching {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Finding songs for you…").color(MUTED));
                });
                ui.add_space(8.);
                ui.horizontal(|ui| {
                    let count = ((ui.available_width() + 12.) / 148.).floor().max(1.) as usize;
                    let width = (ui.available_width() - (count - 1) as f32 * 12.) / count as f32;
                    ui.spacing_mut().item_spacing.x = 12.;
                    for _ in 0..count {
                        let (r, _) = ui.allocate_exact_size(vec2(width, 126.), Sense::hover());
                        widgets::gradient(ui.painter(), r, 10., SURFACE, HOVER);
                    }
                });
            } else if let Some(error) = &app.recommendations.error {
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1., LINE))
                    .corner_radius(12)
                    .inner_margin(20)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new("Couldn't load your recommendations").font(bold(16.)),
                        );
                        ui.add(egui::Label::new(RichText::new(error).color(MUTED)).wrap());
                        if widgets::button(ui, "Try again", true).clicked() {
                            actions.push(Action::RefreshRecommendations);
                        }
                    });
            } else {
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1., LINE))
                    .corner_radius(12)
                    .inner_margin(20)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(RichText::new("A mix that starts with you").font(bold(17.)));
                        ui.label(
                            RichText::new(
                                "Play a song or like a favorite to discover more of what you love.",
                            )
                            .color(MUTED),
                        );
                        if widgets::button(ui, "Find your first song", true).clicked() {
                            actions.push(Action::FocusSearch);
                        }
                    });
            }
            if !app.recent.is_empty() {
                ui.add_space(16.);
                let mut recent = Carousel::new(ui, "recent_shelf", app.recent.len().min(12), 100.);
                section_header(
                    ui,
                    "Continue listening",
                    "",
                    Some(("Show all", Action::Navigate(View::Library), true)),
                    Some(&mut recent),
                    actions,
                );
                ui.add_space(8.);
                track_shelf(app, ui, actions, false, &recent);
            }
            ui.add_space(16.);
            section_header(
                ui,
                "Your collection",
                "Keep your favorites close",
                Some(("Show all", Action::Navigate(View::Library), true)),
                None,
                actions,
            );
            ui.add_space(8.);
            let width = (ui.available_width() - 12.) / 2.;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.;
                quick_card(
                    ui,
                    width,
                    Icon::Heart,
                    "Liked Music",
                    &format!("{} songs, all yours", app.favorites.len()),
                    Action::Navigate(View::Favorites),
                    actions,
                );
                quick_card(
                    ui,
                    width,
                    Icon::Queue,
                    "Your queue",
                    &format!("{} tracks up next", app.queue_len()),
                    Action::ToggleQueue,
                    actions,
                );
            });
            ui.add_space(16.);
        });
}

fn section_header(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    action: Option<(&str, Action, bool)>,
    carousel: Option<&mut Carousel>,
    actions: &mut Vec<Action>,
) {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 24.;
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).font(bold(23.)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(carousel) = carousel {
                    carousel.controls(ui, title);
                }
                if let Some((label, action, enabled)) = action
                    && ui
                        .add_enabled(
                            enabled,
                            egui::Button::new(RichText::new(label).size(11.).color(MUTED))
                                .frame(false),
                        )
                        .clicked()
                {
                    actions.push(action);
                }
            });
        });
    });
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).size(12.).color(MUTED));
    }
}

fn home_hero(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>, compact: bool) {
    let subtitle_offset = if compact { 111. } else { 139. };
    let subtitle_height = 18.;
    let button_offset = subtitle_offset + subtitle_height + 16.;
    let (hero, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), button_offset + 34. + 16.),
        Sense::hover(),
    );
    app.artwork.paint_hero(ui, hero);
    ui.painter().rect_stroke(
        hero,
        12,
        Stroke::new(1., egui::Color32::from_rgb(100, 37, 64)),
        egui::StrokeKind::Inside,
    );
    let x = hero.left() + 28.;
    let y = hero.top();
    ui.painter().text(
        pos2(x, y + 23.),
        Align2::LEFT_TOP,
        "L I S T E N   W I T H O U T   L I M I T S",
        bold(9.),
        egui::Color32::from_rgb(255, 152, 180),
    );
    for (index, line) in ["Music for", "every moment"].iter().enumerate() {
        ui.painter().text(
            pos2(
                x,
                y + if compact { 36. } else { 43. }
                    + index as f32 * if compact { 35. } else { 43. },
            ),
            Align2::LEFT_TOP,
            *line,
            bold(if compact { 32. } else { 40. }),
            TEXT,
        );
    }
    widgets::text(
        ui,
        Rect::from_min_size(
            pos2(x, y + subtitle_offset),
            vec2(hero.width() - 56., subtitle_height),
        ),
        "Discover new music, curated just for you.",
        13.,
        TEXT,
    );
    let button_y = y + button_offset;
    for (index, label) in ["Play all", "Explore"].iter().enumerate() {
        let r = Rect::from_min_size(pos2(x + index as f32 * 134., button_y), vec2(122., 34.));
        let response = ui.interact(r, ui.id().with(("hero_action", index)), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *label));
        if index == 0 {
            widgets::glow(ui.painter(), r.center(), 12.);
        }
        ui.painter().rect_filled(
            r,
            17,
            if index == 0 {
                ACCENT
            } else {
                egui::Color32::from_rgba_unmultiplied(31, 24, 39, 210)
            },
        );
        if index == 1 {
            ui.painter()
                .rect_stroke(r, 17, Stroke::new(1., LINE), egui::StrokeKind::Inside);
        }
        if index == 0 {
            icons::paint(
                ui.painter(),
                Icon::Play,
                Rect::from_min_size(r.min + vec2(17., 10.), vec2(14., 14.)),
                TEXT,
            );
        }
        ui.painter().text(
            r.center() + vec2(if index == 0 { 9. } else { 0. }, 0.),
            Align2::CENTER_CENTER,
            *label,
            bold(12.),
            TEXT,
        );
        if response.clicked() {
            actions.push(if index == 1 {
                Action::FocusSearch
            } else if !app.recommendations.tracks.is_empty() {
                Action::PlayTracks(app.recommendations.tracks.clone())
            } else if !app.recent.is_empty() {
                Action::PlayTracks(app.recent.clone())
            } else {
                Action::Discover("music mix")
            });
        }
    }
}

fn discovery_shelf(ui: &mut Ui, actions: &mut Vec<Action>, carousel: &Carousel) {
    let width = carousel.width;
    carousel.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.;
            for (i, (title, subtitle, icon, left, right, action)) in [
                (
                    "Discover Mix",
                    "Find your next favorite",
                    Icon::Radio,
                    [24, 62, 194],
                    [161, 18, 180],
                    Action::Discover("new music discoveries"),
                ),
                (
                    "Chill Vibes",
                    "Slow down, tune in",
                    Icon::Music,
                    [32, 91, 127],
                    [26, 35, 61],
                    Action::Discover("chill lofi music"),
                ),
                (
                    "Focus",
                    "In your own world",
                    Icon::Music,
                    [38, 74, 104],
                    [13, 23, 42],
                    Action::Discover("focus instrumental music"),
                ),
                (
                    "Release Radar",
                    "Something new",
                    Icon::Radio,
                    [171, 52, 37],
                    [54, 21, 31],
                    Action::Discover("new music releases"),
                ),
                (
                    "Your Likes",
                    "All your favorites",
                    Icon::Heart,
                    [150, 40, 185],
                    [34, 17, 78],
                    Action::Navigate(View::Favorites),
                ),
                (
                    "Feel Good Pop",
                    "Turn up the mood",
                    Icon::Music,
                    [223, 85, 103],
                    [78, 27, 53],
                    Action::Discover("feel good pop songs"),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let (r, _) = ui.allocate_exact_size(vec2(width, 122.), Sense::hover());
                let response = ui.interact(r, ui.id().with(("discover", i)), Sense::click());
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title)
                });
                let rgb = |c: [u8; 3]| egui::Color32::from_rgb(c[0], c[1], c[2]);
                widgets::gradient(ui.painter(), r, 9., rgb(left), rgb(right));
                let p = ui.painter().with_clip_rect(r.shrink(1.));
                for offset in 0..4 {
                    p.add(egui::epaint::CubicBezierShape::from_points_stroke(
                        [
                            r.min + vec2(-10., 4. + offset as f32 * 10.),
                            r.min + vec2(width * 0.4, 100.),
                            r.min + vec2(width * 0.6, -24.),
                            r.min + vec2(width + 10., 34. + offset as f32 * 10.),
                        ],
                        false,
                        egui::Color32::TRANSPARENT,
                        Stroke::new(1., egui::Color32::from_white_alpha(25)),
                    ));
                }
                icons::paint(
                    &p,
                    icon,
                    Rect::from_center_size(r.center() - vec2(0., 16.), vec2(32., 32.)),
                    egui::Color32::from_white_alpha(215),
                );
                let bottom = Rect::from_min_max(pos2(r.left(), r.bottom() - 46.), r.max);
                widgets::gradient(
                    &p,
                    bottom,
                    0.,
                    egui::Color32::from_black_alpha(155),
                    egui::Color32::from_black_alpha(100),
                );
                widgets::text(
                    ui,
                    Rect::from_min_size(
                        pos2(r.left() + 10., r.bottom() - 44.),
                        vec2(width - 20., 24.),
                    ),
                    title,
                    12.,
                    TEXT,
                );
                widgets::text(
                    ui,
                    Rect::from_min_size(
                        pos2(r.left() + 10., r.bottom() - 23.),
                        vec2(width - 20., 18.),
                    ),
                    subtitle,
                    10.,
                    MUTED,
                );
                ui.painter().rect_stroke(
                    r,
                    9,
                    Stroke::new(
                        1.,
                        if response.hovered() || response.has_focus() {
                            ACCENT
                        } else {
                            LINE
                        },
                    ),
                    egui::StrokeKind::Inside,
                );
                if response.clicked() {
                    actions.push(action);
                }
            }
        });
    });
}

fn track_shelf(
    app: &mut MusicApp,
    ui: &mut Ui,
    actions: &mut Vec<Action>,
    recommended: bool,
    carousel: &Carousel,
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
    let width = carousel.width;
    carousel.show(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 12.;
            for (i, track) in tracks.iter().take(12).enumerate() {
                ui.push_id((id, &track.id, i), |ui| {
                    let height = if recommended { 126. } else { width + 44. };
                    let (r, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
                    let response = ui.interact(r, ui.id().with("play"), Sense::click_and_drag());
                    if response.drag_started() {
                        response.dnd_set_drag_payload(crate::app::TrackDrag {
                            track: track.clone(),
                            origin: crate::app::DragOrigin::Track,
                        });
                    }
                    let selected = selectable && app.selected == Some(i);
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
                        if app.reveal_selection {
                            response.scroll_to_me(Some(egui::Align::Center));
                            app.reveal_selection = false;
                        }
                    }
                    let cover = Rect::from_min_size(
                        r.min,
                        vec2(width, if recommended { height } else { width }),
                    );
                    app.artwork.paint(ui, track, cover, 8);
                    let text_y = if recommended {
                        r.bottom() - 43.
                    } else {
                        cover.bottom() + 3.
                    };
                    let inset = if recommended { 10. } else { 0. };
                    if recommended {
                        ui.painter().rect_filled(
                            Rect::from_min_max(pos2(r.left(), r.bottom() - 48.), r.max).shrink(1.),
                            7,
                            egui::Color32::from_black_alpha(200),
                        );
                        ui.painter().rect_stroke(
                            r,
                            8,
                            Stroke::new(1., if selected { ACCENT } else { LINE }),
                            egui::StrokeKind::Inside,
                        );
                    }
                    widgets::text(
                        ui,
                        Rect::from_min_size(
                            pos2(r.left() + inset, text_y),
                            vec2(width - 2. * inset, 22.),
                        ),
                        &track.title,
                        12.,
                        if app.current.as_ref().is_some_and(|t| t.id == track.id) {
                            ACCENT
                        } else {
                            TEXT
                        },
                    );
                    widgets::text(
                        ui,
                        Rect::from_min_size(
                            pos2(r.left() + inset, text_y + 21.),
                            vec2(width - 2. * inset, 18.),
                        ),
                        &track.artist,
                        10.,
                        MUTED,
                    );
                    if response.hovered() || response.has_focus() {
                        let center = cover.center() - vec2(0., if recommended { 16. } else { 0. });
                        widgets::glow(ui.painter(), center, 18.);
                        ui.painter().circle_filled(center, 19., ACCENT);
                        icons::paint(
                            ui.painter(),
                            Icon::Play,
                            Rect::from_center_size(center, vec2(17., 17.)),
                            TEXT,
                        );
                    }
                    if response.clicked() {
                        if selectable {
                            actions.push(Action::Select(i));
                        }
                        actions.push(Action::Play(track.clone()));
                    }
                    response.context_menu(|ui| {
                        if ui.button("Play").clicked() {
                            actions.push(Action::Play(track.clone()));
                            ui.close();
                        }
                        let liked = app.favorites.iter().any(|t| t.id == track.id);
                        if ui
                            .button(if liked {
                                "Remove from favorites"
                            } else {
                                "Save to favorites"
                            })
                            .clicked()
                        {
                            actions.push(Action::Favorite(track.clone()));
                            ui.close();
                        }
                        if ui.button("Add to queue").clicked() {
                            actions.push(Action::Enqueue(track.clone(), false));
                            ui.close();
                        }
                        if ui.button("Add to playlist…").clicked() {
                            actions.push(Action::AddToPlaylist(track.clone()));
                            ui.close();
                        }
                        if ui.button("Start song radio").clicked() {
                            actions.push(Action::StartRadio(track.clone()));
                            ui.close();
                        }
                    });
                    response.on_hover_text(format!(
                        "{}\n{}\nRight-click for more actions",
                        track.title, track.artist
                    ));
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
    let (r, _) = ui.allocate_exact_size(vec2(width, 72.), Sense::hover());
    let response = ui.interact(r, ui.id().with(title), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, title));
    widgets::gradient(
        ui.painter(),
        r,
        10.,
        if response.hovered() { HOVER } else { ACTIVE },
        SURFACE,
    );
    ui.painter()
        .rect_stroke(r, 10, Stroke::new(1., LINE), egui::StrokeKind::Inside);
    let art = Rect::from_min_size(r.min + vec2(14., 12.), vec2(48., 48.));
    widgets::gradient(
        ui.painter(),
        art,
        8.,
        egui::Color32::from_rgb(129, 39, 169),
        egui::Color32::from_rgb(48, 30, 87),
    );
    icons::paint(ui.painter(), icon, art.shrink(13.), TEXT);
    widgets::text(
        ui,
        Rect::from_min_size(
            pos2(art.right() + 12., r.top() + 14.),
            vec2(width - 86., 22.),
        ),
        title,
        13.,
        TEXT,
    );
    widgets::text(
        ui,
        Rect::from_min_size(
            pos2(art.right() + 12., r.top() + 37.),
            vec2(width - 86., 20.),
        ),
        subtitle,
        11.,
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
            widgets::gradient(
                ui.painter(),
                r,
                16.,
                egui::Color32::from_rgb(150, 40, 185),
                egui::Color32::from_rgb(34, 17, 78),
            );
            icons::paint(ui.painter(), Icon::Heart, r.shrink(25.), TEXT);
            ui.vertical(|ui| {
                widgets::caption(ui, "YOUR COLLECTION");
                widgets::heading(
                    ui,
                    "Liked Music",
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
                        egui::Button::new(
                            RichText::new(label)
                                .size(12.)
                                .color(if tab == app.library_tab { TEXT } else { MUTED }),
                        )
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
    });
}

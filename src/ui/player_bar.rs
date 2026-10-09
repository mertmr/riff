use super::{icons::Icon, theme::*, widgets};
use crate::app::{Action, MusicApp, Playback};
use eframe::egui::{self, Align2, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};

pub fn show(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    egui::Panel::bottom("playback_bar")
        .exact_size(112.)
        .resizable(false)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(SIDEBAR)
                .inner_margin(egui::Margin::symmetric(24, 0)),
        )
        .show(ui, |ui| {
            let r = ui.max_rect();
            ui.painter().hline(
                r.expand2(vec2(24., 0.)).x_range(),
                r.top(),
                Stroke::new(1., LINE),
            );
            let left = (r.width() * 0.29).clamp(240., 368.);
            let right = if r.width() < 1050. { 184. } else { 224. };
            let left_rect = Rect::from_min_size(r.min, vec2(left, r.height()));
            let center = Rect::from_min_max(
                pos2(r.left() + left + 16., r.top()),
                pos2(r.right() - right - 16., r.bottom()),
            );
            let right_rect = Rect::from_min_max(pos2(r.right() - right, r.top()), r.max);
            now_playing(app, ui, left_rect, actions);
            transport(app, ui, center, actions);
            extras(app, ui, right_rect, actions);
        });
}
fn now_playing(app: &mut MusicApp, ui: &mut Ui, r: Rect, actions: &mut Vec<Action>) {
    let art = Rect::from_min_size(pos2(r.left(), r.center().y - 32.), vec2(64., 64.));
    if let Some(track) = &app.current {
        let liked = app.favorites.iter().any(|t| t.id == track.id);
        app.artwork.paint(ui, track, art, 8);
        let text_x = art.right() + 16.;
        let width = r.right() - text_x - 40.;
        widgets::text(
            ui,
            Rect::from_min_size(pos2(text_x, r.center().y - 31.), vec2(width, 24.)),
            &track.title,
            14.,
            TEXT,
        );
        widgets::text(
            ui,
            Rect::from_min_size(pos2(text_x, r.center().y - 7.), vec2(width, 20.)),
            &track.artist,
            12.,
            MUTED,
        );
        let status = if app.recovering {
            "Reconnecting audio…"
        } else if app.buffering {
            "Buffering…"
        } else {
            match app.playback {
                Playback::Loading => "Loading audio…",
                Playback::Paused => "Paused",
                Playback::Error => "Couldn't play · Retry",
                Playback::Stopped => "Playback stopped",
                _ => "Now playing",
            }
        };
        let status_rect = Rect::from_min_size(pos2(text_x, r.center().y + 15.), vec2(width, 20.));
        widgets::text(
            ui,
            status_rect,
            status,
            10.,
            if app.playback == Playback::Error {
                ERROR
            } else {
                DIM
            },
        );
        let response = ui.interact(
            Rect::from_min_size(pos2(text_x, r.center().y - 31.), vec2(width, 64.)),
            ui.id().with("current_title"),
            if app.playback == Playback::Error {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                if app.playback == Playback::Error {
                    egui::WidgetType::Button
                } else {
                    egui::WidgetType::Label
                },
                true,
                format!("{} · {} · {status}", track.title, track.artist),
            )
        });
        if response.clicked() {
            actions.push(Action::TogglePlayback);
        }
        response.on_hover_text(app.playback_error.as_deref().map_or_else(
            || format!("{}\n{}", track.title, track.artist),
            str::to_owned,
        ));
        let favorite = Rect::from_center_size(pos2(r.right() - 16., r.center().y), vec2(32., 40.));
        if widgets::icon_button(
            ui,
            "player_favorite",
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
    } else {
        widgets::placeholder(ui, art, 8, Icon::Music);
        widgets::text(
            ui,
            Rect::from_min_size(
                pos2(art.right() + 16., r.center().y - 24.),
                vec2(r.width() - 80., 24.),
            ),
            "Your soundtrack starts here",
            13.,
            TEXT,
        );
        widgets::text(
            ui,
            Rect::from_min_size(
                pos2(art.right() + 16., r.center().y + 2.),
                vec2(r.width() - 80., 24.),
            ),
            "Find a song to get started",
            11.,
            MUTED,
        );
    }
}
fn transport(app: &MusicApp, ui: &mut Ui, r: Rect, actions: &mut Vec<Action>) {
    let cy = r.top() + 38.;
    let cx = r.center().x;
    let previous = Rect::from_center_size(pos2(cx - 56., cy), vec2(40., 40.));
    let play = Rect::from_center_size(pos2(cx, cy), vec2(40., 40.));
    let next = Rect::from_center_size(pos2(cx + 56., cy), vec2(40., 40.));
    if widgets::icon_button(
        ui,
        "previous",
        previous,
        Icon::Previous,
        "Previous · restart after 3 seconds",
        false,
        app.can_previous(),
        false,
    )
    .clicked()
    {
        actions.push(Action::Previous);
    }
    let loading = app.playback == Playback::Loading;
    if widgets::icon_button(
        ui,
        "play_pause",
        play,
        if app.playback == Playback::Playing {
            Icon::Pause
        } else {
            Icon::Play
        },
        if app.playback == Playback::Error {
            "Retry playback"
        } else if app.playback == Playback::Playing {
            "Pause · Space"
        } else {
            "Play · Space"
        },
        false,
        app.current.is_some() && !loading,
        true,
    )
    .clicked()
    {
        actions.push(Action::TogglePlayback);
    }
    if loading {
        ui.painter().circle_filled(play.center(), 20., SURFACE);
        egui::Spinner::new()
            .size(18.)
            .color(ACCENT)
            .paint_at(ui, play.shrink(10.));
    }
    if widgets::icon_button(
        ui,
        "next",
        next,
        Icon::Next,
        "Next track",
        false,
        app.queue_len() > 0 || app.radio.is_some(),
        false,
    )
    .clicked()
    {
        actions.push(Action::Next);
    }
    let progress_width = r.width().min(560.);
    let progress = Rect::from_center_size(
        pos2(cx, r.top() + 84.),
        vec2((progress_width - 88.).max(64.), 24.),
    );
    let enabled = app.seekable
        && app.duration.is_some()
        && matches!(app.playback, Playback::Playing | Playback::Paused);
    if let Some(value) = widgets::slider(
        ui,
        "seek",
        progress,
        app.position,
        app.duration.unwrap_or(0.),
        enabled,
        true,
        "Seek",
    ) {
        actions.push(Action::Seek(value));
    }
    ui.painter().text(
        pos2(progress.left() - 12., progress.center().y),
        Align2::RIGHT_CENTER,
        widgets::time(app.position),
        FontId::proportional(10.),
        DIM,
    );
    ui.painter().text(
        pos2(progress.right() + 12., progress.center().y),
        Align2::LEFT_CENTER,
        app.duration.map_or_else(|| "—:—".into(), widgets::time),
        FontId::proportional(10.),
        DIM,
    );
}
fn extras(app: &MusicApp, ui: &mut Ui, r: Rect, actions: &mut Vec<Action>) {
    let y = r.center().y;
    let q = Rect::from_center_size(pos2(r.right() - 20., y), vec2(40., 40.));
    if widgets::icon_button(
        ui,
        "player_queue",
        q,
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
    if app.queue_open {
        ui.painter()
            .circle_filled(pos2(q.center().x, q.bottom() + 4.), 2., ACCENT);
    }
    let radio = Rect::from_center_size(pos2(q.center().x, y - 36.), vec2(28., 28.));
    if widgets::icon_button(
        ui,
        "player_radio",
        radio,
        Icon::Radio,
        if app.radio.is_some() {
            "Stop song radio"
        } else {
            "Start song radio"
        },
        app.radio.is_some(),
        app.current.is_some(),
        false,
    )
    .clicked()
    {
        if app.radio.is_some() {
            actions.push(Action::StopRadio);
        } else if let Some(track) = &app.current {
            actions.push(Action::StartRadio(track.clone()));
        }
    }
    let mute = Rect::from_center_size(pos2(r.left() + 16., y), vec2(32., 40.));
    if widgets::icon_button(
        ui,
        "mute",
        mute,
        if app.volume == 0 {
            Icon::Muted
        } else {
            Icon::Volume
        },
        if app.volume == 0 { "Unmute" } else { "Mute" },
        false,
        true,
        false,
    )
    .clicked()
    {
        actions.push(Action::Mute);
    }
    let volume = Rect::from_min_size(
        pos2(mute.right() + 8., y - 12.),
        vec2(q.left() - mute.right() - 24., 24.),
    );
    if let Some(v) = widgets::slider(
        ui,
        "volume",
        volume,
        app.volume as f64,
        100.,
        true,
        false,
        "Volume",
    ) {
        actions.push(Action::Volume(v.round() as u8));
    }
    widgets::text(
        ui,
        Rect::from_center_size(pos2(volume.center().x, y + 28.), vec2(72., 16.)),
        &format!("{}%", app.volume),
        10.,
        DIM,
    );
    if app.buffering {
        widgets::text(
            ui,
            Rect::from_min_size(pos2(r.left(), r.top() + 10.), vec2(r.width(), 16.)),
            "Buffering audio…",
            10.,
            ACCENT,
        );
    }
    // A stop command is useful during slow startup, without adding another transport glyph.
    if app.playback == Playback::Loading {
        let stop = Rect::from_min_size(pos2(r.left(), r.top() + 10.), vec2(104., 20.));
        let response = ui.interact(stop, ui.id().with("cancel_playback"), Sense::click());
        widgets::text(ui, stop, "Cancel loading", 10., MUTED);
        if response.clicked() {
            actions.push(Action::Stop);
        }
    }
}

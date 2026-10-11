use super::{
    icons::Icon,
    theme::*,
    widgets::{self, Row},
};
use crate::app::{Action, MusicApp, Playback};
use egui::{self, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

pub fn show(app: &mut MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    egui::Panel::right("queue_drawer")
        .exact_size(320.)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(SIDEBAR).inner_margin(16))
        .show(ui, |ui| {
            ui.painter().vline(
                ui.max_rect().left() - 16.,
                ui.max_rect().y_range(),
                Stroke::new(1., LINE),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("Your queue").font(bold(22.)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(32., 32.), Sense::hover());
                    if widgets::icon_button(
                        ui,
                        "close_queue",
                        r,
                        Icon::Close,
                        "Close queue · Escape",
                        false,
                        true,
                        false,
                    )
                    .clicked()
                    {
                        actions.push(Action::ToggleQueue);
                    }
                });
            });
            ui.add_space(8.);
            widgets::caption(ui, "NOW PLAYING");
            ui.add_space(8.);
            if let Some(track) = &app.current {
                let (r, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 72.), Sense::hover());
                ui.painter().rect_filled(r, 10, ACTIVE);
                let cover = Rect::from_min_size(r.min + vec2(8., 8.), vec2(56., 56.));
                app.artwork.paint(ui, track, cover, 6);
                widgets::text(
                    ui,
                    Rect::from_min_size(
                        pos2(cover.right() + 12., r.top() + 14.),
                        vec2(r.width() - 88., 24.),
                    ),
                    &track.title,
                    13.,
                    TEXT,
                );
                widgets::text(
                    ui,
                    Rect::from_min_size(
                        pos2(cover.right() + 12., r.top() + 38.),
                        vec2(r.width() - 88., 20.),
                    ),
                    &track.artist,
                    11.,
                    MUTED,
                );
                ui.interact(r, ui.id().with("queue_current"), Sense::hover())
                    .on_hover_text(format!("{}\n{}", track.title, track.artist));
            } else {
                ui.label(RichText::new("Nothing playing yet").size(12.).color(MUTED));
            }
            ui.add_space(8.);
            radio_controls(app, ui, actions);
            ui.add_space(8.);
            ui.horizontal(|ui| {
                widgets::caption(ui, &format!("UP NEXT · {}", app.queue_len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(app.queue_len() > 0, |ui| {
                        if ui.small_button("Clear").clicked() {
                            actions.push(Action::ClearQueue);
                        }
                        if ui.small_button("Save playlist").clicked() {
                            app.playlist_editor = Some(crate::app::PlaylistEditor::Create {
                                title: "Queue".into(),
                                tracks: (0..app.queue_len())
                                    .filter_map(|i| app.queue_track(i).cloned())
                                    .collect(),
                            });
                        }
                    });
                });
            });
            if let Some(i) = app.queue_selected.filter(|i| *i < app.queue_len()) {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("Track {}", i + 1))
                            .size(11.)
                            .color(MUTED),
                    );
                    for (id, icon, tip, enabled, action) in [
                        (
                            "queue_up",
                            Icon::Up,
                            "Move up",
                            app.can_queue_move(i, -1),
                            Action::QueueMove(i, -1),
                        ),
                        (
                            "queue_down",
                            Icon::Down,
                            "Move down",
                            app.can_queue_move(i, 1),
                            Action::QueueMove(i, 1),
                        ),
                        (
                            "queue_remove",
                            Icon::Trash,
                            "Remove track",
                            true,
                            Action::QueueRemove(i),
                        ),
                    ] {
                        let (r, _) = ui.allocate_exact_size(vec2(36., 36.), Sense::hover());
                        if widgets::icon_button(ui, id, r, icon, tip, false, enabled, false)
                            .clicked()
                        {
                            actions.push(action);
                        }
                    }
                });
            } else {
                ui.label(
                    RichText::new("Drag to reorder · Click artwork to play")
                        .size(10.)
                        .color(DIM),
                );
            }
            ui.add_space(12.);
            if app.queue_len() == 0 && app.radio.is_some() {
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "Related songs will appear here. Your queued songs play first.",
                        )
                        .size(12.)
                        .color(MUTED),
                    )
                    .wrap(),
                );
            } else if app.queue_len() == 0 {
                if widgets::empty(
                    ui,
                    Icon::Queue,
                    "Room for more",
                    "Use a track's options to add it to your queue.",
                    "Find music",
                ) {
                    actions.push(Action::FocusSearch);
                }
            } else {
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.;
                    let mut scroll = egui::ScrollArea::vertical()
                        .max_height((ui.available_height() - 52.).max(32.))
                        .min_scrolled_height(32.)
                        .id_salt("queue_scroll")
                        .auto_shrink([false, false]);
                    let last = ui
                        .ctx()
                        .data(|d| d.get_temp::<usize>(ui.id().with("queue_selected")));
                    if last != app.queue_selected {
                        if let Some(i) = app.queue_selected {
                            ui.ctx()
                                .data_mut(|d| d.insert_temp(ui.id().with("queue_selected"), i));
                            let offset = egui::scroll_area::State::load(
                                ui.ctx(),
                                ui.make_persistent_id("queue_scroll"),
                            )
                            .map_or(0., |s| s.offset.y);
                            let y = i as f32 * 64.;
                            let h = (ui.available_height() - 52.).max(32.);
                            scroll = scroll.vertical_scroll_offset(if y < offset {
                                y
                            } else if y + 64. > offset + h {
                                y + 64. - h
                            } else {
                                offset
                            });
                        } else {
                            ui.ctx()
                                .data_mut(|d| d.remove::<usize>(ui.id().with("queue_selected")));
                        }
                    }
                    scroll.show_rows(ui, 64., app.queue_len(), |ui, range| {
                        super::collections::auto_scroll(ui);
                        for i in range {
                            let track = app.queue_track(i).unwrap().clone();
                            let queue_range = 0..app.queue_len();
                            widgets::track_row(
                                ui,
                                &mut app.artwork,
                                Row {
                                    track: &track,
                                    index: i,
                                    current: false,
                                    playing: app.playback == Playback::Playing,
                                    liked: app.favorites.iter().any(|t| t.id == track.id),
                                    selected: app.queue_selected == Some(i),
                                    queue: true,
                                    collection: false,
                                    radio: i >= app.queue.len(),
                                    queue_range,
                                    saved: app.saved_tracks.iter().any(|t| t.id == track.id),
                                    queue_revision: app.queue_revision,
                                    playlist: None,
                                },
                                actions,
                            );
                        }
                    });
                });
            }
            super::collections::drop_zone(
                ui,
                "Drop tracks here to add to queue",
                |drag| Action::QueueDrop(drag, app.queue_len()),
                actions,
            );
        });
}

fn radio_controls(app: &MusicApp, ui: &mut Ui, actions: &mut Vec<Action>) {
    if let Some(radio) = &app.radio {
        egui::Frame::new()
            .fill(ACTIVE)
            .corner_radius(10)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    widgets::caption(ui, "SONG RADIO");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "Stop", false).clicked() {
                            actions.push(Action::StopRadio);
                        }
                    });
                });
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 22.), Sense::hover());
                widgets::text(ui, rect, &radio.seed.title, 12., TEXT);
                ui.interact(rect, ui.id().with("radio_seed"), Sense::hover())
                    .on_hover_text(format!(
                        "Based on {} · {}",
                        radio.seed.title, radio.seed.artist
                    ));
                if radio.searching {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(if radio.waiting {
                                "Finding your next song…"
                            } else {
                                "Finding related songs…"
                            })
                            .size(11.)
                            .color(MUTED),
                        );
                    });
                } else if let Some(error) = &radio.error {
                    egui::ScrollArea::vertical()
                        .id_salt("radio_error")
                        .max_height(72.)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(error).size(11.).color(ERROR))
                                    .wrap(),
                            );
                        });
                    if widgets::button(ui, "Retry radio", false).clicked() {
                        actions.push(Action::RetryRadio);
                    }
                } else {
                    ui.label(
                        RichText::new(format!(
                            "{} related songs · Queued songs play first",
                            radio.upcoming.len()
                        ))
                        .size(10.)
                        .color(MUTED),
                    );
                }
            });
    } else if let Some(track) = &app.current
        && widgets::button(ui, "Start song radio", false).clicked()
    {
        actions.push(Action::StartRadio(track.clone()));
    }
}

mod account;
mod artwork;
mod collections;
pub mod icons;
mod player_bar;
mod queue;
mod screens;
pub mod theme;
pub mod widgets;
use crate::app::{Action, MusicApp};
use eframe::egui::{self, RichText, Stroke};
use theme::*;

pub fn show(app: &mut MusicApp, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    player_bar::show(app, ui, &mut actions);
    let width = ui.available_width();
    let compact = app.queue_open && width < 1180.;
    egui::Panel::left("sidebar")
        .exact_size(if compact {
            80.
        } else if width < 1150. {
            192.
        } else {
            216.
        })
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(SIDEBAR).inner_margin(16))
        .show(ui, |ui| screens::sidebar(app, ui, compact, &mut actions));
    if app.queue_open {
        queue::show(app, ui, &mut actions);
    }
    let padding = if ui.available_width() < 750. { 24 } else { 32 };
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(BG).inner_margin(padding))
        .show(ui, |ui| {
            screens::topbar(app, ui, &mut actions);
            ui.add_space(24.);
            if let Some(error) = &app.global_error {
                egui::Frame::new()
                    .fill(SURFACE)
                    .inner_margin(12)
                    .corner_radius(8)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(egui::Label::new(RichText::new(error).color(ERROR)).wrap());
                            if ui.small_button("Dismiss").clicked() {
                                actions.push(Action::DismissError);
                            }
                        });
                    });
                ui.add_space(16.);
            }
            screens::content(app, ui, &mut actions);
        });
    if let Some(notice) = &app.notice {
        egui::Area::new(egui::Id::new("notice"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0., -126.))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(HOVER)
                    .stroke(Stroke::new(1., LINE))
                    .corner_radius(12)
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&notice.text).size(13.));
                            let (r, _) =
                                ui.allocate_exact_size(egui::vec2(32., 32.), egui::Sense::hover());
                            if widgets::icon_button(
                                ui,
                                "dismiss_notice",
                                r,
                                icons::Icon::Close,
                                "Dismiss notification",
                                false,
                                true,
                                false,
                            )
                            .clicked()
                            {
                                actions.push(Action::DismissNotice);
                            }
                        });
                    });
            });
    }
    collections::editor(app, ui.ctx(), &mut actions);
    if let Some(drag) = egui::DragAndDrop::payload::<crate::app::TrackDrag>(ui.ctx()) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        if let Some(pointer) = ui.ctx().pointer_interact_pos() {
            egui::Area::new(egui::Id::new("track_drag_preview"))
                .order(egui::Order::Tooltip)
                .interactable(false)
                .fixed_pos(pointer + egui::vec2(16., 16.))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(240.);
                        ui.label(&drag.track.title);
                    });
                });
        }
    }
    let ctx = ui.ctx().clone();
    for action in actions {
        app.dispatch(action, &ctx);
    }
}

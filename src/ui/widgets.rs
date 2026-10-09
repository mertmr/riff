pub use super::artwork::Artwork;
use super::{
    icons::{self, Icon},
    theme::*,
};
use crate::{app::Action, model::Track};
use eframe::egui::{
    self, Color32, FontId, Rect, Response, RichText, Sense, Stroke, Ui, pos2, vec2,
};

pub fn placeholder(ui: &Ui, rect: Rect, radius: u8, icon: Icon) {
    ui.painter().rect_filled(rect, radius, HOVER);
    icons::paint(
        ui.painter(),
        icon,
        Rect::from_center_size(rect.center(), rect.size() * 0.38),
        DIM,
    );
}
pub fn text(ui: &Ui, rect: Rect, value: &str, size: f32, color: Color32) {
    let mut job = egui::text::LayoutJob::simple(
        value.to_owned(),
        FontId::proportional(size),
        color,
        rect.width(),
    );
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(rect.left(), rect.center().y - galley.size().y / 2.),
        galley,
        color,
    );
}
pub fn caption(ui: &mut Ui, value: &str) {
    ui.label(RichText::new(value).font(bold(11.)).color(MUTED));
}
pub fn heading(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).font(bold(32.)));
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).size(14.).color(MUTED));
    }
}
pub fn button(ui: &mut Ui, text: &str, primary: bool) -> Response {
    ui.scope(|ui| {
        let v = ui.visuals_mut();
        v.widgets.inactive.weak_bg_fill = if primary { ACCENT } else { HOVER };
        v.widgets.hovered.weak_bg_fill = if primary { TEXT } else { LINE };
        v.widgets.active.weak_bg_fill = if primary {
            ACCENT.gamma_multiply(0.9)
        } else {
            ACTIVE
        };
        ui.add(
            egui::Button::new(
                RichText::new(text)
                    .color(if primary { SIDEBAR } else { TEXT })
                    .font(bold(14.)),
            )
            .stroke(Stroke::NONE)
            .corner_radius(20),
        )
    })
    .inner
}
// The explicit state flags keep custom control behavior consistent across every screen.
#[allow(clippy::too_many_arguments)]
pub fn icon_button(
    ui: &mut Ui,
    id: impl egui::AsIdSalt,
    rect: Rect,
    icon: Icon,
    tip: &str,
    active: bool,
    enabled: bool,
    filled: bool,
) -> Response {
    let response = ui.interact(
        rect,
        ui.id().with(id),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, tip));
    let hovered = enabled && (response.hovered() || response.has_focus());
    let color = if !enabled {
        DIM
    } else if filled {
        SIDEBAR
    } else if active {
        ACCENT
    } else if hovered {
        TEXT
    } else {
        MUTED
    };
    if filled {
        ui.painter().circle_filled(
            rect.center(),
            rect.width() / 2.,
            if hovered { TEXT } else { ACCENT },
        );
    } else if hovered {
        ui.painter().rect_filled(rect, 8, HOVER);
    }
    icons::paint(
        ui.painter(),
        icon,
        Rect::from_center_size(rect.center(), vec2(20., 20.)),
        color,
    );
    if response.has_focus() {
        ui.painter()
            .rect_stroke(rect, 8, Stroke::new(1., ACCENT), egui::StrokeKind::Inside);
    }
    response.on_hover_text(tip)
}

#[allow(clippy::too_many_arguments)]
pub fn slider(
    ui: &Ui,
    id: impl egui::AsIdSalt,
    rect: Rect,
    value: f64,
    max: f64,
    enabled: bool,
    commit: bool,
    label: &str,
) -> Option<f64> {
    let id = ui.id().with(id);
    let response = ui.interact(
        rect,
        id,
        if enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    response.widget_info(|| egui::WidgetInfo::slider(enabled, value, label));
    let dragged = response.dragged() || response.drag_stopped();
    let mut next = if dragged {
        ui.ctx().data(|d| d.get_temp::<f64>(id)).unwrap_or(value)
    } else {
        value
    };
    if enabled
        && (response.clicked() || response.dragged() || response.drag_stopped())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        next = ((pointer.x - rect.left()) / rect.width()).clamp(0., 1.) as f64 * max;
        ui.ctx().data_mut(|d| d.insert_temp(id, next));
    }
    let keyboard = if enabled && response.has_focus() {
        ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
                1.
            } else if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
                -1.
            } else {
                0.
            }
        })
    } else {
        0.
    };
    if keyboard != 0. {
        next = (next + keyboard * (max / 20.).min(5.)).clamp(0., max);
    }
    let accessible = if enabled {
        ui.input(|input| {
            use egui::accesskit::{Action, ActionData};
            input
                .accesskit_action_requests(id, Action::SetValue)
                .find_map(|request| {
                    if let Some(ActionData::NumericValue(v)) = request.data {
                        Some(v)
                    } else {
                        None
                    }
                })
        })
    } else {
        None
    }
    .filter(|v| v.is_finite());
    if let Some(v) = accessible {
        next = v.clamp(0., max);
    }
    ui.ctx().accesskit_node_builder(id, |node| {
        node.set_min_numeric_value(0.);
        node.set_max_numeric_value(max);
        if enabled {
            node.add_action(egui::accesskit::Action::SetValue);
        }
    });
    let ratio = if max > 0. {
        (next / max).clamp(0., 1.)
    } else {
        0.
    };
    let y = rect.center().y;
    ui.painter().line_segment(
        [pos2(rect.left(), y), pos2(rect.right(), y)],
        Stroke::new(3., LINE),
    );
    if enabled || value > 0. {
        ui.painter().line_segment(
            [
                pos2(rect.left(), y),
                pos2(rect.left() + rect.width() * ratio as f32, y),
            ],
            Stroke::new(
                3.,
                if label != "Volume" || response.hovered() || dragged {
                    ACCENT
                } else {
                    MUTED
                },
            ),
        );
    }
    if enabled && (response.hovered() || dragged || response.has_focus()) {
        ui.painter()
            .circle_filled(pos2(rect.left() + rect.width() * ratio as f32, y), 5., TEXT);
    }
    response.clone().on_hover_text(if enabled {
        if label == "Volume" {
            format!("Volume: {}%", next.round() as u8)
        } else {
            format!("{label}: {}", time(next))
        }
    } else {
        "Available once audio is ready".into()
    });
    if enabled
        && ((response.clicked() || response.drag_stopped() || (!commit && response.dragged()))
            || keyboard != 0.
            || accessible.is_some())
    {
        Some(next)
    } else {
        None
    }
}
pub fn time(seconds: f64) -> String {
    let s = seconds.max(0.) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

pub struct Row<'a> {
    pub track: &'a Track,
    pub index: usize,
    pub current: bool,
    pub playing: bool,
    pub liked: bool,
    pub selected: bool,
    pub queue: bool,
    pub radio: bool,
    pub queue_range: std::ops::Range<usize>,
}
pub fn track_row(ui: &mut Ui, art: &mut Artwork, row: Row<'_>, actions: &mut Vec<Action>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let response = ui.interact(rect, ui.id().with(("row", row.index)), Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            row.selected,
            format!("{} · {}", row.track.title, row.track.artist),
        )
    });
    if row.selected {
        ui.ctx()
            .data_mut(|d| d.insert_temp(egui::Id::new("selected_row_focus"), response.id));
    }
    let hovered = response.hovered();
    if row.current || row.selected || hovered {
        ui.painter()
            .rect_filled(rect, 8, if row.current { ACTIVE } else { HOVER });
    }
    if row.selected {
        ui.painter().rect_stroke(
            rect,
            8,
            Stroke::new(1., ACCENT.gamma_multiply(0.45)),
            egui::StrokeKind::Inside,
        );
    }
    let narrow = rect.width() < 480.;
    let cover = Rect::from_min_size(pos2(rect.left() + 8., rect.top() + 8.), vec2(48., 48.));
    art.paint(ui, row.track, cover, 6);
    let play = ui.interact(cover, ui.id().with(("cover", row.index)), Sense::click());
    play.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("Play {}", row.track.title),
        )
    });
    if play.hovered() || hovered || row.current {
        ui.painter()
            .rect_filled(cover, 6, Color32::from_black_alpha(145));
        icons::paint(
            ui.painter(),
            if row.current && row.playing {
                Icon::Pause
            } else {
                Icon::Play
            },
            cover.shrink(13.),
            TEXT,
        );
    }
    if play.clicked() {
        actions.push(if row.current {
            Action::TogglePlayback
        } else if row.queue {
            Action::QueuePlay(row.index)
        } else {
            Action::Play(row.track.clone())
        });
    }
    let more = Rect::from_min_size(pos2(rect.right() - 40., rect.top() + 12.), vec2(32., 40.));
    let more_response = icon_button(
        ui,
        ("more", row.index),
        more,
        Icon::More,
        "Track options",
        false,
        true,
        false,
    );
    let favorite = Rect::from_min_size(pos2(rect.right() - 80., rect.top() + 12.), vec2(32., 40.));
    if icon_button(
        ui,
        ("like", row.index),
        favorite,
        if row.liked { Icon::Check } else { Icon::Heart },
        if row.liked {
            "Remove from favorites"
        } else {
            "Save to favorites"
        },
        row.liked,
        true,
        false,
    )
    .clicked()
    {
        actions.push(Action::Favorite(row.track.clone()));
    }
    let text_end = rect.right() - if narrow { 88. } else { 152. };
    let title = Rect::from_min_max(
        pos2(cover.right() + 16., rect.top() + 10.),
        pos2(text_end, rect.top() + 32.),
    );
    let artist = title.translate(vec2(0., 24.));
    text(ui, title, &row.track.title, 14., TEXT);
    let artist_label = if row.radio {
        format!("Radio · {}", row.track.artist)
    } else {
        row.track.artist.clone()
    };
    text(ui, artist, &artist_label, 12., MUTED);
    if !narrow {
        text(
            ui,
            Rect::from_min_size(pos2(rect.right() - 144., rect.top() + 20.), vec2(48., 24.)),
            &row.track.duration_label(),
            12.,
            MUTED,
        );
    }
    if response.clicked() && !play.clicked() && !more_response.clicked() {
        actions.push(if row.queue {
            Action::QueueSelect(row.index)
        } else {
            Action::Select(row.index)
        });
        if !row.queue {
            actions.push(if row.current {
                Action::TogglePlayback
            } else {
                Action::Play(row.track.clone())
            });
        }
    }
    if row.queue && response.double_clicked() {
        actions.push(Action::QueuePlay(row.index));
    }
    let menu = |ui: &mut Ui, actions: &mut Vec<Action>| {
        ui.set_min_width(184.);
        if ui.button("Play now").clicked() {
            actions.push(if row.queue {
                Action::QueuePlay(row.index)
            } else {
                Action::Play(row.track.clone())
            });
            ui.close();
        }
        if ui.button("Start song radio").clicked() {
            actions.push(Action::StartRadio(row.track.clone()));
            ui.close();
        }
        if ui.button("Play next").clicked() {
            actions.push(Action::Enqueue(row.track.clone(), true));
            ui.close();
        }
        if ui.button("Add to queue").clicked() {
            actions.push(Action::Enqueue(row.track.clone(), false));
            ui.close();
        }
        if ui
            .button(if row.liked {
                "Remove from favorites"
            } else {
                "Save to favorites"
            })
            .clicked()
        {
            actions.push(Action::Favorite(row.track.clone()));
            ui.close();
        }
        if row.queue {
            ui.separator();
            if ui
                .add_enabled(
                    row.index > row.queue_range.start,
                    egui::Button::new("Move up"),
                )
                .clicked()
            {
                actions.push(Action::QueueMove(row.index, -1));
                ui.close();
            }
            if ui
                .add_enabled(
                    row.index + 1 < row.queue_range.end,
                    egui::Button::new("Move down"),
                )
                .clicked()
            {
                actions.push(Action::QueueMove(row.index, 1));
                ui.close();
            }
            if ui.button("Remove from queue").clicked() {
                actions.push(Action::QueueRemove(row.index));
                ui.close();
            }
        }
    };
    egui::Popup::menu(&more_response).show(|ui| menu(ui, actions));
    response.clone().context_menu(|ui| menu(ui, actions));
    if hovered {
        response.on_hover_text(format!("{}\n{}", row.track.title, row.track.artist));
    }
    if row.selected
        && ui.input(|i| i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::ArrowUp))
    {
        ui.scroll_to_rect(rect, Some(egui::Align::Center));
    }
}

pub fn empty(ui: &mut Ui, icon: Icon, title: &str, description: &str, action: &str) -> bool {
    let compact = ui.available_height() < 300.;
    egui::ScrollArea::vertical()
        .id_salt(("empty", title))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(if compact { 16. } else { 40. });
            ui.vertical_centered(|ui| {
                let size = if compact { 56. } else { 72. };
                let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
                ui.painter()
                    .circle_filled(rect.center(), size / 2., SURFACE);
                icons::paint(ui.painter(), icon, rect.shrink(size * 0.30), ACCENT);
                ui.add_space(16.);
                ui.label(RichText::new(title).font(bold(22.)));
                ui.add_space(4.);
                ui.add(egui::Label::new(RichText::new(description).color(MUTED)).wrap());
                ui.add_space(16.);
                !action.is_empty() && button(ui, action, true).clicked()
            })
            .inner
        })
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessible_sliders_commit_real_values_and_respect_disabled_state() {
        let ctx = egui::Context::default();
        let mut id = egui::Id::NULL;
        let mut value = None;
        let mut frame = |events, enabled| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(400., 200.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    id = ui.id().with("test_volume");
                    let (rect, _) = ui.allocate_exact_size(vec2(160., 40.), Sense::hover());
                    value = slider(ui, "test_volume", rect, 20., 100., enabled, true, "Volume");
                },
            );
            output.textures_delta.clear();
            (id, value)
        };
        let (id, _) = frame(Vec::new(), true);
        let request = |value| {
            egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::SetValue,
                target_tree: egui::accesskit::TreeId::ROOT,
                target_node: id.accesskit_id(),
                data: Some(egui::accesskit::ActionData::NumericValue(value)),
            })
        };
        assert_eq!(frame(vec![request(42.)], true).1, Some(42.));
        assert_eq!(frame(vec![request(120.)], true).1, Some(100.));
        assert_eq!(frame(vec![request(42.)], false).1, None);
        assert_eq!(frame(vec![request(f64::NAN)], true).1, None);
    }
}

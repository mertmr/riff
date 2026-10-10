use super::{icons::Icon, widgets};
use eframe::egui::{self, Id, Sense, Ui, vec2};

const GAP: f32 = 12.;

pub(super) struct Carousel {
    salt: &'static str,
    id: Id,
    pub width: f32,
    page: f32,
    max_offset: f32,
    delta: f32,
}

impl Carousel {
    pub fn new(ui: &Ui, salt: &'static str, cards: usize, target_width: f32) -> Self {
        let available = ui.available_width().max(1.);
        let visible = ((available + GAP) / (target_width + GAP)).floor().max(1.);
        let width = (available - GAP * (visible - 1.)) / visible;
        let content_width = cards as f32 * width + cards.saturating_sub(1) as f32 * GAP;
        Self {
            salt,
            id: ui.make_persistent_id(egui::IdSalt::new(salt)),
            width,
            page: visible * (width + GAP),
            max_offset: (content_width - available).max(0.),
            delta: 0.,
        }
    }

    // Called in the heading's right-to-left layout, before its optional action.
    pub fn controls(&mut self, ui: &mut Ui, title: &str) {
        if self.max_offset <= 1. {
            return;
        }
        let offset = egui::scroll_area::State::load(ui.ctx(), self.id)
            .map_or(0., |state| state.offset.x)
            .clamp(0., self.max_offset);
        for (name, icon, direction, enabled) in [
            (
                "Next",
                Icon::ChevronRight,
                1.,
                offset < self.max_offset - 1.,
            ),
            ("Previous", Icon::ChevronLeft, -1., offset > 1.),
        ] {
            let (rect, _) = ui.allocate_exact_size(vec2(28., 28.), Sense::hover());
            if widgets::icon_button(
                ui,
                (self.salt, name),
                rect,
                icon,
                &format!("{name} · {title}"),
                false,
                enabled,
                false,
            )
            .clicked()
            {
                let target = (offset + direction * self.page).clamp(0., self.max_offset);
                self.delta = offset - target;
            }
        }
    }

    pub fn show<R>(
        &self,
        ui: &mut Ui,
        content: impl FnOnce(&mut Ui) -> R,
    ) -> egui::scroll_area::ScrollAreaOutput<R> {
        let previous_offset =
            egui::scroll_area::State::load(ui.ctx(), self.id).map_or(0., |state| state.offset.x);
        let output = egui::ScrollArea::horizontal()
            .id_salt(self.salt)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if self.delta != 0. {
                    ui.scroll_with_delta(vec2(self.delta, 0.));
                }
                content(ui)
            });
        // Headings are drawn first, so refresh their controls after scrolling settles.
        if output.state.offset.x != previous_offset {
            ui.ctx().request_repaint();
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Pos2, RawInput, Rect, accesskit};

    struct Frame {
        output: egui::FullOutput,
        offsets: [f32; 2],
        viewport: Rect,
        page: f32,
        max_offset: f32,
    }

    fn render(ctx: &egui::Context, time: f64, events: Vec<Event>, cards: usize) -> Frame {
        let mut offsets = [0.; 2];
        let mut viewport = Rect::NOTHING;
        let mut page = 0.;
        let mut max_offset = 0.;
        let mut output = ctx.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(500., 340.))),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (index, salt) in ["first_shelf", "second_shelf"].into_iter().enumerate() {
                        let mut carousel = Carousel::new(ui, salt, cards, 100.);
                        ui.horizontal(|ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    carousel.controls(ui, salt);
                                },
                            );
                        });
                        let top = ui.next_widget_position().y;
                        let scroll = carousel.show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = GAP;
                                for _ in 0..cards {
                                    ui.allocate_exact_size(
                                        vec2(carousel.width, 80.),
                                        Sense::click(),
                                    );
                                }
                            });
                        });
                        assert_eq!(scroll.id, carousel.id);
                        offsets[index] = scroll.state.offset.x;
                        if index == 0 {
                            viewport = Rect::from_min_size(
                                scroll.inner_rect.min,
                                vec2(
                                    scroll.inner_rect.width(),
                                    ui.next_widget_position().y - top - ui.spacing().item_spacing.y,
                                ),
                            );
                            page = carousel.page;
                            max_offset = carousel.max_offset;
                        }
                    }
                });
            },
        );
        output.textures_delta.clear();
        Frame {
            output,
            offsets,
            viewport,
            page,
            max_offset,
        }
    }

    fn control(frame: &Frame, label: &str) -> (accesskit::NodeId, bool) {
        let (id, node) = frame
            .output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .unwrap();
        (*id, !node.is_disabled())
    }

    fn click(id: accesskit::NodeId) -> Event {
        Event::AccessKitActionRequest(accesskit::ActionRequest {
            action: accesskit::Action::Click,
            target_tree: accesskit::TreeId::ROOT,
            target_node: id,
            data: None,
        })
    }

    #[test]
    fn arrows_page_one_shelf_and_stop_at_both_ends() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let start = render(&ctx, 0., vec![], 14);
        let (next, enabled) = control(&start, "Next · first_shelf");
        let (previous, previous_enabled) = control(&start, "Previous · first_shelf");
        assert!(enabled);
        assert!(!previous_enabled);
        assert!((start.page - start.viewport.width() - GAP).abs() < 0.1);

        render(&ctx, 0.1, vec![click(next)], 14);
        let first_page = render(&ctx, 1., vec![], 14);
        assert!((first_page.offsets[0] - start.page).abs() < 0.1);
        assert_eq!(first_page.offsets[1], 0.);
        assert!(control(&render(&ctx, 1.01, vec![], 14), "Previous · first_shelf").1);

        render(&ctx, 1.1, vec![click(next)], 14);
        render(&ctx, 2., vec![], 14);
        render(&ctx, 2.1, vec![click(next)], 14);
        let end = render(&ctx, 3., vec![], 14);
        assert!((end.offsets[0] - start.max_offset).abs() < 0.1);
        assert!(!control(&render(&ctx, 3.01, vec![], 14), "Next · first_shelf").1);
        assert_eq!(
            render(&ctx, 3.1, vec![click(next)], 14).offsets[0],
            end.offsets[0]
        );

        for step in 0..3 {
            render(&ctx, 4. + step as f64, vec![click(previous)], 14);
            render(&ctx, 4.9 + step as f64, vec![], 14);
        }
        let back = render(&ctx, 8., vec![], 14);
        assert_eq!(back.offsets, [0.; 2]);
        assert!(!control(&back, "Previous · first_shelf").1);
    }

    #[test]
    fn hidden_scrollbars_keep_trackpad_scrolling_and_controls_follow_it() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        render(&ctx, 0., vec![], 14);
        let start = render(&ctx, 0.01, vec![], 14);
        render(
            &ctx,
            0.1,
            vec![
                Event::PointerMoved(start.viewport.center()),
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(-48., 0.),
                    modifiers: egui::Modifiers::NONE,
                    phase: egui::TouchPhase::Move,
                },
            ],
            14,
        );
        let scrolled = render(&ctx, 1., vec![], 14);
        assert!(scrolled.offsets[0] > 0.);
        assert_eq!(scrolled.offsets[1], 0.);
        assert!(control(&scrolled, "Previous · first_shelf").1);
        assert_eq!(scrolled.viewport.height(), 80.);

        let resized = render(&ctx, 2., vec![], 3);
        assert_eq!(resized.offsets, [0.; 2]);
        assert!(
            !resized
                .output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| node
                    .label()
                    .is_some_and(|label| label.starts_with("Next ·")))
        );
    }
}

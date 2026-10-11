//! Resolution-independent icons with consistent strokes and optical sizes.
use egui::{self, Color32, Painter, Rect, Shape, Stroke, pos2};

#[derive(Clone, Copy)]
pub enum Icon {
    Bell,
    Home,
    Search,
    Library,
    Heart,
    Queue,
    Radio,
    Play,
    Pause,
    Previous,
    Next,
    Volume,
    Muted,
    Close,
    More,
    Up,
    Down,
    ChevronLeft,
    ChevronRight,
    Trash,
    Music,
    Check,
    Arrow,
}

pub fn paint(p: &Painter, icon: Icon, rect: Rect, color: Color32) {
    let r = Rect::from_center_size(
        rect.center(),
        egui::Vec2::splat(rect.width().min(rect.height())),
    );
    let pt = |x: f32, y: f32| {
        pos2(
            r.left() + x / 24.0 * r.width(),
            r.top() + y / 24.0 * r.height(),
        )
    };
    let stroke = Stroke::new((r.width() / 24.0 * 1.7).max(1.2), color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([pt(a.0, a.1), pt(b.0, b.1)], stroke);
    };
    let path = |points: &[(f32, f32)]| {
        p.add(Shape::line(
            points.iter().map(|&(x, y)| pt(x, y)).collect(),
            stroke,
        ));
    };
    match icon {
        Icon::Bell => {
            path(&[
                (4., 18.),
                (6., 15.),
                (6., 9.),
                (7., 6.),
                (10., 4.),
                (14., 4.),
                (17., 6.),
                (18., 9.),
                (18., 15.),
                (20., 18.),
                (4., 18.),
            ]);
            p.circle_filled(pt(12., 2.), r.width() / 16., color);
            path(&[(10., 21.), (12., 22.), (14., 21.)]);
        }
        Icon::Radio => {
            p.circle_filled(pt(12., 12.), r.width() / 10., color);
            for x in [5., 1.] {
                path(&[(x + 2., 5. + x / 2.), (x, 12.), (x + 2., 19. - x / 2.)]);
                path(&[
                    (22. - x, 5. + x / 2.),
                    (24. - x, 12.),
                    (22. - x, 19. - x / 2.),
                ]);
            }
        }
        Icon::Home => {
            path(&[
                (3., 10.),
                (12., 3.),
                (21., 10.),
                (21., 21.),
                (15., 21.),
                (15., 14.),
                (9., 14.),
                (9., 21.),
                (3., 21.),
                (3., 10.),
            ]);
        }
        Icon::Search => {
            p.circle_stroke(pt(10.5, 10.5), r.width() * 0.29, stroke);
            line((16., 16.), (21., 21.));
        }
        Icon::Library => {
            line((4., 4.), (4., 21.));
            line((9., 4.), (9., 21.));
            line((14., 5.), (20., 20.));
        }
        Icon::Heart => {
            for curve in [
                [(12., 21.), (10., 19.), (2., 13.), (2., 8.)],
                [(2., 8.), (2., 3.), (8., 1.), (12., 6.)],
                [(12., 6.), (16., 1.), (22., 3.), (22., 8.)],
                [(22., 8.), (22., 13.), (14., 19.), (12., 21.)],
            ] {
                p.add(egui::epaint::CubicBezierShape::from_points_stroke(
                    curve.map(|(x, y)| pt(x, y)),
                    false,
                    Color32::TRANSPARENT,
                    stroke,
                ));
            }
        }
        Icon::Queue => {
            for y in [5., 11., 17.] {
                line((3., y), (14., y));
            }
            p.add(Shape::convex_polygon(
                vec![pt(18., 12.), pt(23., 16.), pt(18., 20.)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(
                vec![pt(8., 4.), pt(21., 12.), pt(8., 20.)],
                color,
                Stroke::NONE,
            ));
        }
        Icon::Pause => {
            for x in [7., 15.] {
                p.rect_filled(Rect::from_min_max(pt(x, 5.), pt(x + 3., 19.)), 1, color);
            }
        }
        Icon::Previous | Icon::Next => {
            let flip = matches!(icon, Icon::Previous);
            let x = |x: f32| if flip { 24. - x } else { x };
            p.add(Shape::convex_polygon(
                vec![pt(x(5.), 5.), pt(x(16.), 12.), pt(x(5.), 19.)],
                color,
                Stroke::NONE,
            ));
            line((x(19.), 5.), (x(19.), 19.));
        }
        Icon::Volume | Icon::Muted => {
            path(&[
                (3., 9.),
                (7., 9.),
                (12., 5.),
                (12., 19.),
                (7., 15.),
                (3., 15.),
                (3., 9.),
            ]);
            if matches!(icon, Icon::Muted) {
                line((16., 9.), (22., 15.));
                line((16., 15.), (22., 9.));
            } else {
                path(&[(16., 8.), (18., 10.), (18., 14.), (16., 16.)]);
                path(&[(19., 4.), (22., 8.), (23., 12.), (22., 16.), (19., 20.)]);
            }
        }
        Icon::Close => {
            line((6., 6.), (18., 18.));
            line((18., 6.), (6., 18.));
        }
        Icon::More => {
            for x in [5., 12., 19.] {
                p.circle_filled(pt(x, 12.), r.width() / 12., color);
            }
        }
        Icon::Up => path(&[(6., 15.), (12., 9.), (18., 15.)]),
        Icon::Down => path(&[(6., 9.), (12., 15.), (18., 9.)]),
        Icon::ChevronLeft => path(&[(15., 6.), (9., 12.), (15., 18.)]),
        Icon::ChevronRight => path(&[(9., 6.), (15., 12.), (9., 18.)]),
        Icon::Trash => {
            line((4., 6.), (20., 6.));
            path(&[(8., 6.), (8., 3.), (16., 3.), (16., 6.)]);
            path(&[(6., 6.), (7., 21.), (17., 21.), (18., 6.)]);
            line((10., 10.), (10., 17.));
            line((14., 10.), (14., 17.));
        }
        Icon::Music => {
            path(&[(9., 17.), (9., 5.), (21., 3.), (21., 15.)]);
            line((9., 9.), (21., 7.));
            p.circle_stroke(pt(6., 18.), r.width() / 8., stroke);
            p.circle_stroke(pt(18., 16.), r.width() / 8., stroke);
        }
        Icon::Arrow => {
            line((4., 12.), (20., 12.));
            path(&[(14., 6.), (20., 12.), (14., 18.)]);
        }
        Icon::Check => path(&[(5., 12.), (10., 17.), (20., 6.)]),
    }
}

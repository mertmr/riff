use eframe::egui::{self, Color32, FontData, FontFamily, FontId, Stroke, TextStyle};

// YouTube-inspired dark palette. Keep surfaces neutral and reserve red for accents.
pub const BG: Color32 = Color32::from_rgb(15, 15, 15);
pub const SIDEBAR: Color32 = BG;
pub const SURFACE: Color32 = Color32::from_rgb(33, 33, 33);
pub const HOVER: Color32 = Color32::from_rgb(48, 48, 48);
pub const ACTIVE: Color32 = HOVER;
pub const ACCENT: Color32 = Color32::from_rgb(255, 0, 51);
pub const TEXT: Color32 = Color32::WHITE;
pub const MUTED: Color32 = Color32::from_rgb(170, 170, 170);
pub const DIM: Color32 = Color32::from_rgb(128, 128, 128);
pub const LINE: Color32 = Color32::from_rgb(63, 63, 63);
pub const ERROR: Color32 = Color32::from_rgb(255, 153, 170);

pub fn install(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".into(),
        FontData::from_static(include_bytes!("../../assets/fonts/InterVariable.ttf")).into(),
    );
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .insert(0, "Inter".into());
    fonts.font_data.insert(
        "Inter Semibold".into(),
        FontData::from_static(include_bytes!("../../assets/fonts/Inter-SemiBold.otf")).into(),
    );
    fonts.families.insert(
        FontFamily::Name("Inter Semibold".into()),
        vec!["Inter Semibold".into(), "Inter".into()],
    );
    ctx.set_fonts(fonts);
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = SIDEBAR;
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACTIVE;
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    v.window_corner_radius = 12.into();
    v.window_stroke = Stroke::new(1.0, LINE);
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.corner_radius = 8.into();
        widget.bg_stroke = Stroke::NONE;
        widget.weak_bg_fill = SURFACE;
        widget.fg_stroke = Stroke::new(1.5, TEXT);
    }
    v.widgets.inactive.bg_fill = SURFACE;
    v.widgets.hovered.bg_fill = HOVER;
    v.widgets.active.bg_fill = ACTIVE;
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(16.0, 10.0);
        s.spacing.interact_size = egui::vec2(40.0, 40.0);
        s.animation_time = 0.14;
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles
            .insert(TextStyle::Heading, FontId::proportional(30.0));
    });
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("Inter Semibold".into()))
}

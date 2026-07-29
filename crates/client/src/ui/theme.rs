use eframe::egui;

pub const CLR_BG: egui::Color32 = egui::Color32::from_rgb(13, 17, 23);
pub const CLR_SURFACE: egui::Color32 = egui::Color32::from_rgb(22, 27, 34);
pub const CLR_SURFACE2: egui::Color32 = egui::Color32::from_rgb(30, 37, 46);
pub const CLR_ACCENT: egui::Color32 = egui::Color32::from_rgb(255, 140, 0);
pub const CLR_GREEN: egui::Color32 = egui::Color32::from_rgb(35, 197, 94);
pub const CLR_RED: egui::Color32 = egui::Color32::from_rgb(218, 54, 51);
pub const CLR_BLUE: egui::Color32 = egui::Color32::from_rgb(88, 166, 255);
pub const CLR_YELLOW: egui::Color32 = egui::Color32::from_rgb(210, 153, 34);
pub const CLR_TEXT: egui::Color32 = egui::Color32::from_rgb(230, 237, 243);
pub const CLR_TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(139, 148, 158);
pub const CLR_BORDER: egui::Color32 = egui::Color32::from_rgb(48, 54, 61);

pub fn apply_theme(ctx: &egui::Context, is_dark: bool) {
    let vis = if is_dark {
        let mut v = egui::Visuals::dark();
        v.panel_fill = CLR_BG;
        v.window_fill = CLR_SURFACE;
        v.window_rounding = egui::Rounding::same(10.0);
        v.widgets.noninteractive.bg_fill = CLR_SURFACE;
        v.widgets.inactive.bg_fill = CLR_SURFACE2;
        v.widgets.hovered.bg_fill = egui::Color32::from_rgb(40, 48, 60);
        v.widgets.active.bg_fill = egui::Color32::from_rgb(50, 60, 75);
        v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, CLR_TEXT_DIM);
        v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, CLR_TEXT);
        v.extreme_bg_color = egui::Color32::from_rgb(10, 14, 20);
        v
    } else {
        let mut v = egui::Visuals::light();
        v.panel_fill = egui::Color32::from_rgb(246, 248, 250);
        v.window_fill = egui::Color32::WHITE;
        v.window_rounding = egui::Rounding::same(10.0);
        v.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(240, 243, 246);
        v.widgets.inactive.bg_fill = egui::Color32::from_rgb(230, 235, 240);
        v.widgets.hovered.bg_fill = egui::Color32::from_rgb(220, 227, 235);
        v.widgets.active.bg_fill = egui::Color32::from_rgb(200, 210, 222);
        v.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(100, 110, 120));
        v.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(30, 35, 42));
        v.extreme_bg_color = egui::Color32::from_rgb(255, 255, 255);
        v
    };
    ctx.set_visuals(vis);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}

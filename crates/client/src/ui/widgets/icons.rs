use eframe::egui;

pub fn name_color(name: &str) -> egui::Color32 {
    let h = name.bytes().fold(5381u32, |a, b| a.wrapping_mul(33).wrapping_add(b as u32));
    let hue = (h % 360) as f32;
    let s = 0.60f32;
    let v = 0.70f32;
    let c2 = v * s;
    let x = c2 * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = v - c2;
    let (r, g, b) = match hue as u32 {
        0..=59 => (c2, x, 0.0),
        60..=119 => (x, c2, 0.0),
        120..=179 => (0.0, c2, x),
        180..=239 => (0.0, x, c2),
        240..=299 => (x, 0.0, c2),
        _ => (c2, 0.0, x),
    };
    egui::Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

pub fn draw_chebu_icon(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let r = size * 0.35;
    let ear_r = size * 0.22;
    let brown = egui::Color32::from_rgb(166, 110, 71);
    let bg = egui::Color32::from_rgb(188, 230, 255);
    p.circle_filled(c, size * 0.5, bg);
    p.circle_filled(c + egui::vec2(-r * 0.9, -r * 0.1), ear_r, brown);
    p.circle_filled(c + egui::vec2(r * 0.9, -r * 0.1), ear_r, brown);
    p.circle_filled(c + egui::vec2(0.0, r * 0.05), r, brown);
    let face = egui::Color32::from_rgb(245, 214, 184);
    p.circle_filled(c + egui::vec2(0.0, r * 0.15), r * 0.75, face);
    p.circle_filled(c + egui::vec2(-r * 0.3, -r * 0.1), r * 0.18, egui::Color32::from_rgb(40, 20, 10));
    p.circle_filled(c + egui::vec2(r * 0.3, -r * 0.1), r * 0.18, egui::Color32::from_rgb(40, 20, 10));
}

pub fn draw_chebu_icon_large(ui: &mut egui::Ui, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    let c = rect.center();
    let r = size * 0.32;
    let ear_r = size * 0.2;
    let brown = egui::Color32::from_rgb(166, 110, 71);
    let lt_brown = egui::Color32::from_rgb(190, 133, 92);
    let bg_col = egui::Color32::from_rgb(140, 195, 230);
    p.circle_filled(c, size * 0.5, bg_col);
    p.circle_filled(c + egui::vec2(-r, -r * 0.05), ear_r, brown);
    p.circle_filled(c + egui::vec2(r, -r * 0.05), ear_r, brown);
    p.circle_filled(c + egui::vec2(-r, -r * 0.05), ear_r * 0.75, lt_brown);
    p.circle_filled(c + egui::vec2(r, -r * 0.05), ear_r * 0.75, lt_brown);
    for i in 1..=2u8 {
        let rr = ear_r * (0.4 + i as f32 * 0.25);
        let stroke = egui::Stroke::new(2.0_f32, egui::Color32::WHITE);
        let lc = c + egui::vec2(-r, -r * 0.05);
        let rc_pos = c + egui::vec2(r, -r * 0.05);
        let segments = 10;
        for s in 0..segments {
            let a1 = -0.8 + (s as f32 / segments as f32) * 1.6;
            let a2 = -0.8 + ((s + 1) as f32 / segments as f32) * 1.6;
            let p1 = lc + egui::vec2(a1.cos() * rr, a1.sin() * rr);
            let p2 = lc + egui::vec2(a2.cos() * rr, a2.sin() * rr);
            p.line_segment([p1, p2], stroke);
        }
        let a_base = std::f32::consts::PI;
        for s in 0..segments {
            let a1 = a_base - 0.8 + (s as f32 / segments as f32) * 1.6;
            let a2 = a_base - 0.8 + ((s + 1) as f32 / segments as f32) * 1.6;
            let p1 = rc_pos + egui::vec2(a1.cos() * rr, a1.sin() * rr);
            let p2 = rc_pos + egui::vec2(a2.cos() * rr, a2.sin() * rr);
            p.line_segment([p1, p2], stroke);
        }
    }
    p.circle_filled(c + egui::vec2(0.0, r * 0.1), r, brown);
    let face = egui::Color32::from_rgb(245, 214, 184);
    p.circle_filled(c + egui::vec2(0.0, r * 0.2), r * 0.72, face);
    let eye_l = c + egui::vec2(-r * 0.28, r * 0.0);
    let eye_r = c + egui::vec2(r * 0.28, r * 0.0);
    p.circle_filled(eye_l, r * 0.17, egui::Color32::from_rgb(40, 20, 10));
    p.circle_filled(eye_r, r * 0.17, egui::Color32::from_rgb(40, 20, 10));
    p.circle_filled(eye_l + egui::vec2(r * 0.05, -r * 0.04), r * 0.07, egui::Color32::WHITE);
    p.circle_filled(eye_r + egui::vec2(r * 0.05, -r * 0.04), r * 0.07, egui::Color32::WHITE);
    p.circle_filled(c + egui::vec2(0.0, r * 0.25), r * 0.1, egui::Color32::from_rgb(40, 20, 10));
}

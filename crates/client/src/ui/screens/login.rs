use crate::app::App;
use crate::ui::theme::*;
use crate::ui::widgets::icons::draw_chebu_icon_large;
use eframe::egui;

pub fn draw_login(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    let avail = ui.available_size();
    ui.allocate_ui_at_rect(
        egui::Rect::from_center_size(
            ui.min_rect().center() + egui::vec2(0.0, -20.0),
            egui::vec2(320.0, avail.y),
        ),
        |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                draw_chebu_icon_large(ui, 72.0);
                ui.add_space(12.0);
                ui.label(egui::RichText::new("Cheburgram").size(26.0).strong().color(CLR_ACCENT));
                ui.label(egui::RichText::new("Голосовой мессенджер").small().color(CLR_TEXT_DIM));
                ui.add_space(28.0);

                egui::Frame::none()
                    .fill(CLR_SURFACE)
                    .rounding(egui::Rounding::same(12.0))
                    .inner_margin(egui::Margin::same(20.0))
                    .stroke(egui::Stroke::new(1.0_f32, CLR_BORDER))
                    .show(ui, |ui| {
                        ui.set_width(280.0);
                        ui.label(egui::RichText::new("Ваше имя").color(CLR_TEXT_DIM).small());
                        ui.add_space(4.0);
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut app.name_input)
                                .hint_text("Например: Amer")
                                .desired_width(f32::INFINITY)
                                .font(egui::FontId::proportional(15.0))
                                .text_color(CLR_TEXT),
                        );
                        if resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                            app.connect_register();
                        }

                        ui.add_space(12.0);
                        ui.label(egui::RichText::new("Адрес сервера").color(CLR_TEXT_DIM).small());
                        ui.add_space(4.0);
                        ui.add(
                            egui::TextEdit::singleline(&mut app.cfg.server_address)
                                .hint_text("ip:7880")
                                .desired_width(f32::INFINITY)
                                .text_color(CLR_TEXT),
                        );

                        ui.add_space(16.0);
                        let btn = egui::Button::new(
                            egui::RichText::new("Войти").size(15.0).strong().color(egui::Color32::WHITE),
                        )
                        .fill(CLR_ACCENT)
                        .min_size(egui::vec2(f32::INFINITY, 40.0));
                        if ui.add(btn).clicked() {
                            app.connect_register();
                        }
                    });

                if !app.status.is_empty() {
                    ui.add_space(12.0);
                    ui.label(egui::RichText::new(&app.status).small().color(CLR_TEXT_DIM));
                }
            });
        },
    );
}

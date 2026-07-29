use crate::app::App;
use crate::ui::theme::*;
use crate::ui::widgets::icons::name_color;
use eframe::egui;

pub fn draw_calling(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context, to_name: &str) {
    let avail = ui.available_size();
    let target_w = (340.0f32).min(avail.x - 20.0);
    ui.allocate_ui_at_rect(
        egui::Rect::from_center_size(ui.min_rect().center(), egui::vec2(target_w, avail.y)),
        |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                let t = ctx.input(|i| i.time) as f32;
                let pulse = (t * 2.5).sin() * 0.15 + 0.85;
                let col = name_color(to_name);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 80.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 40.0 * pulse, col.gamma_multiply(0.3));
                ui.painter().circle_filled(rect.center(), 40.0, col);
                let first = to_name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    first.to_string(),
                    egui::FontId::proportional(38.0),
                    egui::Color32::WHITE,
                );

                ui.add_space(16.0);
                ui.label(egui::RichText::new(to_name).size(22.0).strong().color(CLR_TEXT));
                ui.add_space(6.0);
                let dots = match ((t * 2.0) as u32) % 4 {
                    0 => ".",
                    1 => "..",
                    2 => "...",
                    _ => "",
                };
                ui.label(egui::RichText::new(format!("Вызов{}", dots)).color(CLR_TEXT_DIM));
                ui.add_space(36.0);

                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Отмена").strong().color(egui::Color32::WHITE),
                        )
                        .fill(CLR_RED)
                        .min_size(egui::vec2(140.0, 42.0)),
                    )
                    .clicked()
                {
                    app.end_call();
                }
            });
        },
    );
}

pub fn draw_incoming(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context, from_peer_id: u32, from_name: &str) {
    let avail = ui.available_size();
    let target_w = (340.0f32).min(avail.x - 20.0);
    ui.allocate_ui_at_rect(
        egui::Rect::from_center_size(ui.min_rect().center(), egui::vec2(target_w, avail.y)),
        |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                let t = ctx.input(|i| i.time) as f32;
                let pulse = (t * 3.0).sin() * 0.2 + 0.8;
                let col = name_color(from_name);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 90.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 45.0 * pulse, CLR_ACCENT.gamma_multiply(0.25));
                ui.painter().circle_filled(rect.center(), 45.0, col);
                let first = from_name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    first.to_string(),
                    egui::FontId::proportional(44.0),
                    egui::Color32::WHITE,
                );

                ui.add_space(14.0);
                ui.label(egui::RichText::new("Входящий звонок").small().color(CLR_TEXT_DIM));
                ui.add_space(4.0);
                ui.label(egui::RichText::new(from_name).size(26.0).strong().color(CLR_TEXT));
                ui.add_space(32.0);

                ui.horizontal(|ui| {
                    let n1 = from_name.to_string();
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Принять").strong().color(egui::Color32::WHITE),
                            )
                            .fill(CLR_GREEN)
                            .min_size(egui::vec2(130.0, 44.0)),
                        )
                        .clicked()
                    {
                        app.accept_call(from_peer_id, n1);
                    }
                    ui.add_space(16.0);
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Отклонить").strong().color(egui::Color32::WHITE),
                            )
                            .fill(CLR_RED)
                            .min_size(egui::vec2(130.0, 44.0)),
                        )
                        .clicked()
                    {
                        app.reject_call(from_peer_id);
                    }
                });
            });
        },
    );
}

pub fn draw_in_call(app: &mut App, ui: &mut egui::Ui, peer_name: &str, started_at: std::time::Instant) {
    let avail = ui.available_size();
    let target_w = (360.0f32).min(avail.x - 20.0);
    ui.allocate_ui_at_rect(
        egui::Rect::from_center_size(ui.min_rect().center(), egui::vec2(target_w, avail.y)),
        |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(16.0);

                let col = name_color(peer_name);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(68.0, 68.0), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), 34.0, col);
                let first = peer_name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    first.to_string(),
                    egui::FontId::proportional(34.0),
                    egui::Color32::WHITE,
                );

                ui.add_space(8.0);
                ui.label(egui::RichText::new(peer_name).size(20.0).strong().color(CLR_TEXT));
                let el = started_at.elapsed().as_secs();
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("{:02}:{:02}", el / 60, el % 60))
                            .size(13.0)
                            .color(CLR_TEXT_DIM),
                    );
                    ui.add_space(6.0);
                    let und = app.audio_underruns();
                    let (q_text, q_col) = if und == 0 {
                        ("📶 HD Связь", CLR_GREEN)
                    } else if und < 5 {
                        ("📶 Хорошая", CLR_YELLOW)
                    } else {
                        ("⚠️ Пропуски", CLR_RED)
                    };
                    ui.label(egui::RichText::new(q_text).small().color(q_col));
                });

                ui.add_space(20.0);

                // VU-метр и статистика
                egui::Frame::none()
                    .fill(CLR_SURFACE)
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::same(14.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let lvl = app.mic_level();
                        let val = lvl as f32 / 100.0;
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Микрофон").small().color(CLR_TEXT_DIM));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let active_col = if lvl > 5 { CLR_GREEN } else { CLR_TEXT_DIM };
                                ui.label(egui::RichText::new(format!("{}%", lvl)).small().color(active_col));
                            });
                        });
                        ui.add_space(4.0);
                        let (vu_rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 12.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(vu_rect, egui::Rounding::same(6.0), CLR_SURFACE2);
                        let fill_w = (vu_rect.width() * val).max(0.0);
                        if fill_w > 0.0 {
                            let fill_rect = egui::Rect::from_min_size(
                                vu_rect.min,
                                egui::vec2(fill_w, vu_rect.height()),
                            );
                            let bar_col =
                                if val > 0.8 { CLR_RED } else if val > 0.5 { CLR_ACCENT } else { CLR_GREEN };
                            ui.painter().rect_filled(fill_rect, egui::Rounding::same(6.0), bar_col);
                        }

                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(format!(
                                "Отпр: {}  Получ: {}  Пропуски: {}",
                                app.pkts_sent(),
                                app.pkts_recv(),
                                app.audio_underruns()
                            ))
                            .small()
                            .color(CLR_TEXT_DIM),
                        );

                        if let Some(err) = app.audio_error() {
                            ui.add_space(6.0);
                            ui.label(egui::RichText::new(format!("⚠ {}", err)).small().color(CLR_YELLOW));
                        }

                        ui.add_space(8.0);
                        // громкость собеседника
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Громкость").small().color(CLR_TEXT_DIM));
                            let mut v = app.peer_volume();
                            if ui
                                .add(egui::Slider::new(&mut v, 0.0..=1.5).show_value(false))
                                .changed()
                            {
                                app.set_peer_volume(v);
                            }
                            ui.label(
                                egui::RichText::new(format!("{:.0}%", v * 100.0)).small().color(CLR_TEXT_DIM),
                            );
                        });
                    });

                ui.add_space(16.0);

                // Управление звонком
                egui::Frame::none()
                    .fill(CLR_SURFACE)
                    .rounding(egui::Rounding::same(10.0))
                    .inner_margin(egui::Margin::same(14.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let mic_muted = app.mic_muted();
                            let mic_txt = if mic_muted { "Мик ВЫКЛ" } else { "Мик вкл" };
                            let mic_col =
                                if mic_muted { egui::Color32::from_rgb(80, 30, 30) } else { CLR_SURFACE2 };
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new(mic_txt).color(CLR_TEXT).strong())
                                        .fill(mic_col)
                                        .min_size(egui::vec2(80.0, 34.0)),
                                )
                                .clicked()
                            {
                                app.toggle_mic();
                            }

                            let snd_muted = app.sound_muted();
                            let spk_txt = if snd_muted { "Звук ВЫКЛ" } else { "Звук вкл" };
                            let spk_col =
                                if snd_muted { egui::Color32::from_rgb(80, 30, 30) } else { CLR_SURFACE2 };
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new(spk_txt).color(CLR_TEXT).strong())
                                        .fill(spk_col)
                                        .min_size(egui::vec2(80.0, 34.0)),
                                )
                                .clicked()
                            {
                                app.toggle_sound();
                            }

                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Завершить").strong().color(egui::Color32::WHITE),
                                    )
                                    .fill(CLR_RED)
                                    .min_size(egui::vec2(90.0, 34.0)),
                                )
                                .clicked()
                            {
                                app.end_call();
                            }
                        });
                    });
            });
        },
    );
}

use crate::app::App;
use crate::config::save_config;
use crate::ui::theme::*;
use eframe::egui;

pub fn draw_settings(app: &mut App, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Настройки").size(15.0).strong().color(CLR_TEXT));
    ui.add_space(8.0);

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        settings_section(ui, "Профиль", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Имя:").color(CLR_TEXT_DIM));
                ui.add(
                    egui::TextEdit::singleline(&mut app.name_input)
                        .desired_width(160.0)
                        .text_color(CLR_TEXT),
                );
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Сохранить").color(egui::Color32::WHITE))
                            .fill(CLR_ACCENT),
                    )
                    .clicked()
                {
                    let new_name = app.name_input.trim().to_string();
                    if !new_name.is_empty() {
                        let changed = app.cfg.display_name != new_name;
                        app.cfg.display_name = new_name;
                        save_config(&app.cfg);
                        if changed {
                            app.restart_net();
                        }
                        app.status = "Имя обновлено".into();
                    }
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Ваш ID:").color(CLR_TEXT_DIM));
                ui.label(
                    egui::RichText::new(&app.cfg.user_code)
                        .strong()
                        .color(CLR_ACCENT)
                        .font(egui::FontId::monospace(14.0)),
                );
            });
        });

        ui.add_space(8.0);

        settings_section(ui, "Сервер", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Адрес:").color(CLR_TEXT_DIM));
                ui.add(
                    egui::TextEdit::singleline(&mut app.cfg.server_address)
                        .desired_width(180.0)
                        .text_color(CLR_TEXT),
                );
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Применить").color(egui::Color32::WHITE))
                            .fill(CLR_ACCENT),
                    )
                    .clicked()
                {
                    save_config(&app.cfg);
                    app.restart_net();
                    app.status = "Переподключение к серверу...".into();
                }
            });

            if let Some(fp) = &app.cfg.server_fingerprint {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("TLS Отпечаток:").small().color(CLR_TEXT_DIM));
                    ui.label(
                        egui::RichText::new(fp)
                            .small()
                            .color(CLR_GREEN)
                            .font(egui::FontId::monospace(11.0)),
                    );
                });
            }
        });

        ui.add_space(8.0);

        settings_section(ui, "Аудио устройства", |ui| {
            let prev_in = app.devs.sel_in;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Микрофон:").color(CLR_TEXT_DIM));
                egui::ComboBox::from_id_source("mic_sel")
                    .selected_text(app.devs.inputs.get(app.devs.sel_in).cloned().unwrap_or_default())
                    .show_ui(ui, |ui| {
                        for (i, n) in app.devs.inputs.clone().iter().enumerate() {
                            ui.selectable_value(&mut app.devs.sel_in, i, n);
                        }
                    });
            });

            if prev_in != app.devs.sel_in && app.mic_test_active {
                app.stop_mic_test();
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let test_col = if app.mic_test_active { CLR_RED } else { CLR_GREEN };
                let test_txt = if app.mic_test_active { "Остановить тест" } else { "Тест микрофона" };
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(test_txt).color(egui::Color32::WHITE).strong(),
                        )
                        .fill(test_col)
                        .min_size(egui::vec2(160.0, 32.0)),
                    )
                    .clicked()
                {
                    if app.mic_test_active {
                        app.stop_mic_test();
                    } else {
                        app.start_mic_test();
                    }
                }
            });

            if app.mic_test_active {
                ui.add_space(6.0);
                let lvl = app.mic_test_level.load(std::sync::atomic::Ordering::Relaxed);
                let val = lvl as f32 / 100.0;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Уровень:").small().color(CLR_TEXT_DIM));
                    let (vu, _) =
                        ui.allocate_exact_size(egui::vec2(180.0, 12.0), egui::Sense::hover());
                    ui.painter().rect_filled(vu, egui::Rounding::same(6.0), CLR_SURFACE2);
                    let fw = (vu.width() * val).max(0.0);
                    if fw > 0.0 {
                        let fr = egui::Rect::from_min_size(vu.min, egui::vec2(fw, vu.height()));
                        let c = if val > 0.8 { CLR_RED } else if val > 0.5 { CLR_ACCENT } else { CLR_GREEN };
                        ui.painter().rect_filled(fr, egui::Rounding::same(6.0), c);
                    }
                    ui.label(egui::RichText::new(format!("{}%", lvl)).small().color(CLR_TEXT_DIM));
                });
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Усиление микрофона:").color(CLR_TEXT_DIM));
                let mut gain = app.cfg.mic_gain;
                if ui
                    .add(egui::Slider::new(&mut gain, 0.5..=3.0).text("x").show_value(true))
                    .changed()
                {
                    app.set_mic_gain(gain);
                    save_config(&app.cfg);
                }
            });

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Динамики:").color(CLR_TEXT_DIM));
                egui::ComboBox::from_id_source("out_sel")
                    .selected_text(
                        app.devs.outputs.get(app.devs.sel_out).cloned().unwrap_or_default(),
                    )
                    .show_ui(ui, |ui| {
                        for (i, n) in app.devs.outputs.clone().iter().enumerate() {
                            ui.selectable_value(&mut app.devs.sel_out, i, n);
                        }
                    });
            });

            ui.add_space(8.0);
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new("Сохранить аудио").color(egui::Color32::WHITE),
                    )
                    .fill(CLR_ACCENT),
                )
                .clicked()
            {
                app.cfg.selected_input = app.devs.sel_in;
                app.cfg.selected_output = app.devs.sel_out;
                save_config(&app.cfg);
                app.status = "Настройки аудио сохранены".into();
            }
        });

        ui.add_space(8.0);

        settings_section(ui, "Тема оформления", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Режим:").color(CLR_TEXT_DIM));
                let dark_bg = if app.cfg.dark_mode { CLR_ACCENT } else { CLR_SURFACE2 };
                let light_bg = if !app.cfg.dark_mode { CLR_ACCENT } else { CLR_SURFACE2 };

                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("🌙 Тёмная").color(egui::Color32::WHITE))
                            .fill(dark_bg),
                    )
                    .clicked()
                {
                    app.cfg.dark_mode = true;
                    save_config(&app.cfg);
                }
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("☀️ Светлая").color(egui::Color32::WHITE))
                            .fill(light_bg),
                    )
                    .clicked()
                {
                    app.cfg.dark_mode = false;
                    save_config(&app.cfg);
                }
            });
        });

        ui.add_space(8.0);

        settings_section(ui, "Масштаб интерфейса", |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Размер UI:").color(CLR_TEXT_DIM));
                if ui
                    .add(egui::Slider::new(&mut app.cfg.zoom_factor, 0.8..=1.6).text("x"))
                    .changed()
                {
                    save_config(&app.cfg);
                }
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let btn_scale = |ui: &mut egui::Ui, app: &mut App, scale: f32, label: &str| {
                    let is_active = (app.cfg.zoom_factor - scale).abs() < 0.05;
                    let bg = if is_active { CLR_ACCENT } else { CLR_SURFACE2 };
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
                                .fill(bg),
                        )
                        .clicked()
                    {
                        app.cfg.zoom_factor = scale;
                        save_config(&app.cfg);
                    }
                };
                btn_scale(ui, app, 0.8, "80%");
                btn_scale(ui, app, 1.0, "100%");
                btn_scale(ui, app, 1.25, "125%");
                btn_scale(ui, app, 1.5, "150%");
            });
        });
    });
}

fn settings_section(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CLR_SURFACE)
        .rounding(egui::Rounding::same(10.0))
        .stroke(egui::Stroke::new(1.0_f32, CLR_BORDER))
        .inner_margin(egui::Margin::same(14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).strong().color(CLR_TEXT));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            content(ui);
        });
}

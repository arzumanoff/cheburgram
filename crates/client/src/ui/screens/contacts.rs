use crate::app::App;
use crate::ui::theme::*;
use crate::ui::widgets::icons::name_color;
use eframe::egui;

fn sanitize_id_input(s: &mut String) {
    let filtered: String = s.chars().filter(|c| c.is_ascii_digit()).take(6).collect();
    if filtered != *s {
        *s = filtered;
    }
}

pub fn draw_contacts(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.add_space(6.0);

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        // Мой ID
        egui::Frame::none()
            .fill(CLR_SURFACE)
            .rounding(egui::Rounding::same(10.0))
            .stroke(egui::Stroke::new(1.0_f32, CLR_BORDER))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Мой ID:").color(CLR_TEXT_DIM));
                    ui.label(
                        egui::RichText::new(&app.cfg.user_code)
                            .size(17.0)
                            .strong()
                            .color(CLR_ACCENT)
                            .font(egui::FontId::monospace(17.0)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let txt = if app.copied_code_banner { "Скопировано!" } else { "📋 Скопировать" };
                        if ui
                            .add(
                                egui::Button::new(egui::RichText::new(txt).small().color(CLR_TEXT))
                                    .fill(CLR_SURFACE2),
                            )
                            .clicked()
                        {
                            ctx.output_mut(|o| o.copied_text = app.cfg.user_code.clone());
                            app.copied_code_banner = true;
                        }
                    });
                });
            });

        ui.add_space(8.0);

        // Запрос в друзья
        egui::Frame::none()
            .fill(CLR_SURFACE)
            .rounding(egui::Rounding::same(10.0))
            .stroke(egui::Stroke::new(1.0_f32, CLR_BORDER))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("➕ Запрос по ID:").color(CLR_TEXT_DIM));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut app.add_friend_input)
                            .hint_text("6 цифр")
                            .desired_width(110.0)
                            .font(egui::FontId::monospace(14.0))
                            .text_color(CLR_TEXT),
                    );
                    sanitize_id_input(&mut app.add_friend_input);

                    let send = resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter));
                    if send && app.add_friend_input.len() == 6 {
                        let code = app.add_friend_input.clone();
                        app.add_friend_input.clear();
                        app.send_friend_request(&code);
                    }
                    let ready = app.add_friend_input.len() == 6;
                    let btn = egui::Button::new(
                        egui::RichText::new("Отправить запрос").strong().color(egui::Color32::WHITE),
                    )
                    .fill(if ready { CLR_ACCENT } else { CLR_SURFACE2 });
                    if ui.add_enabled(ready, btn).on_hover_text("или Enter").clicked() {
                        let code = app.add_friend_input.clone();
                        app.add_friend_input.clear();
                        app.send_friend_request(&code);
                    }
                });
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "Можно просто вставить ID из буфера (Ctrl+V) — лишние символы отфильтруются",
                    )
                    .small()
                    .color(CLR_TEXT_DIM),
                );
            });

        // Входящие заявки
        if !app.pending_friend_requests.is_empty() {
            ui.add_space(10.0);
            ui.label(egui::RichText::new("📩 Запросы в друзья").size(15.0).strong().color(CLR_ACCENT));
            ui.add_space(4.0);

            let requests = app.pending_friend_requests.clone();
            for req in &requests {
                egui::Frame::none()
                    .fill(CLR_SURFACE)
                    .rounding(egui::Rounding::same(10.0))
                    .stroke(egui::Stroke::new(1.0_f32, CLR_ACCENT))
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&req.from_name).size(15.0).strong().color(CLR_TEXT));
                                ui.label(
                                    egui::RichText::new(format!("ID: {}", req.from_code)).small().color(CLR_TEXT_DIM),
                                );
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let fc1 = req.from_code.clone();
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("Принять").strong().color(egui::Color32::WHITE),
                                        )
                                        .fill(CLR_GREEN),
                                    )
                                    .clicked()
                                {
                                    app.accept_friend_request(fc1);
                                }
                                ui.add_space(4.0);
                                let fc2 = req.from_code.clone();
                                if ui
                                    .add(
                                        egui::Button::new(egui::RichText::new("Отклонить").color(CLR_TEXT))
                                            .fill(CLR_SURFACE2),
                                    )
                                    .clicked()
                                {
                                    app.reject_friend_request(fc2);
                                }
                            });
                        });
                    });
                ui.add_space(4.0);
            }
        }

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Мои друзья").size(15.0).strong().color(CLR_TEXT));
        ui.add_space(4.0);

        if app.cfg.friends.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(30.0);
                ui.label(egui::RichText::new("Список друзей пуст").size(16.0).color(CLR_TEXT_DIM));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Введите 6-значный ID друга выше, чтобы отправить запрос")
                        .small()
                        .color(CLR_TEXT_DIM),
                );
            });
            return;
        }

        let friends = app.cfg.friends.clone();
        let statuses = app.friend_statuses.clone();
        let mut call_action: Option<(String, String)> = None;
        let mut remove_action: Option<String> = None;

        for f in &friends {
            let st = statuses.get(&f.user_code);
            let is_online = st.map(|s| s.is_online).unwrap_or(false);

            egui::Frame::none()
                .fill(CLR_SURFACE)
                .rounding(egui::Rounding::same(10.0))
                .stroke(egui::Stroke::new(1.0_f32, CLR_BORDER))
                .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let col = name_color(&f.name);
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                        ui.painter().circle_filled(rect.center(), 20.0, col);
                        let first = f.name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            first.to_string(),
                            egui::FontId::proportional(20.0),
                            egui::Color32::WHITE,
                        );

                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&f.name).size(15.0).strong().color(CLR_TEXT));
                            ui.horizontal(|ui| {
                                let dot_col = if is_online { CLR_GREEN } else { CLR_TEXT_DIM };
                                let (dot, _) =
                                    ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                                ui.painter().circle_filled(dot.center(), 3.0, dot_col);
                                let txt = if is_online { "в сети" } else { "не в сети" };
                                ui.label(
                                    egui::RichText::new(format!("{} • ID {}", txt, f.user_code))
                                        .small()
                                        .color(dot_col),
                                );
                            });
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new("🗑").small().color(CLR_TEXT_DIM))
                                        .fill(egui::Color32::TRANSPARENT),
                                )
                                .on_hover_text("Удалить из друзей")
                                .clicked()
                            {
                                remove_action = Some(f.user_code.clone());
                            }
                            ui.add_space(4.0);

                            let btn_call = egui::Button::new(
                                egui::RichText::new("📞 Позвонить").strong().color(egui::Color32::WHITE),
                            )
                            .fill(if is_online { CLR_GREEN } else { CLR_SURFACE2 });
                            if ui.add_enabled(is_online, btn_call).clicked() {
                                call_action = Some((f.user_code.clone(), f.name.clone()));
                            }
                            ui.add_space(4.0);

                            let btn_chat = egui::Button::new(
                                egui::RichText::new("💬 Чат").strong().color(egui::Color32::WHITE),
                            )
                            .fill(CLR_BLUE);
                            if ui.add(btn_chat).clicked() {
                                app.open_chat(f.user_code.clone(), f.name.clone());
                            }
                        });
                    });
                });
            ui.add_space(6.0);
        }

        if let Some(code) = remove_action {
            app.remove_friend(&code);
        }
        if let Some((code, name)) = call_action {
            app.call_user(code, name);
        }
    });
}

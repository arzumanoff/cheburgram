use crate::app::App;
use crate::ui::theme::*;
use eframe::egui;

pub fn draw_chat_modal(app: &mut App, ctx: &egui::Context) {
    let friend = match app.chat_active_friend.clone() {
        Some(f) => f,
        None => return,
    };

    let screen_w = ctx.screen_rect().width();
    let screen_h = ctx.screen_rect().height();

    let mut open = true;
    egui::Window::new(format!("💬 Чат с {} (ID: {})", friend.name, friend.user_code))
        .open(&mut open)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 10.0))
        .resizable(true)
        .collapsible(false)
        .default_size(egui::vec2(380.0, 480.0))
        .max_size(egui::vec2((screen_w - 16.0).max(280.0), (screen_h - 60.0).max(300.0)))
        .min_size(egui::vec2(280.0, 320.0))
        .show(ctx, |ui| {
            let msgs = app.chat_messages.entry(friend.user_code.clone()).or_default().clone();

            // панель действий: позвонить / добавить в друзья — в один клик
            egui::TopBottomPanel::top("chat_actions_panel")
                .frame(egui::Frame::none().fill(CLR_SURFACE).inner_margin(egui::Margin::same(8.0)))
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        let is_friend =
                            app.cfg.friends.iter().any(|f| f.user_code == friend.user_code);
                        let is_online = app
                            .friend_statuses
                            .get(&friend.user_code)
                            .map(|s| s.is_online)
                            .unwrap_or(false);
                        let can_call = !is_friend || is_online;

                        let btn_call = egui::Button::new(
                            egui::RichText::new("📞 Позвонить").strong().color(egui::Color32::WHITE),
                        )
                        .fill(if is_online { CLR_GREEN } else { CLR_SURFACE2 });
                        if ui
                            .add_enabled(can_call, btn_call)
                            .on_hover_text(if is_online { "Начать звонок" } else { "Собеседник не в сети" })
                            .clicked()
                        {
                            app.call_user(friend.user_code.clone(), friend.name.clone());
                        }

                        if !is_friend {
                            ui.add_space(6.0);
                            let btn_add = egui::Button::new(
                                egui::RichText::new("➕ В друзья").strong().color(egui::Color32::WHITE),
                            )
                            .fill(CLR_ACCENT);
                            if ui.add(btn_add).clicked() {
                                app.send_friend_request(&friend.user_code);
                            }
                        }
                    });
                });

            egui::TopBottomPanel::bottom("chat_input_panel")
                .frame(egui::Frame::none().fill(CLR_SURFACE).inner_margin(egui::Margin::same(8.0)))
                .show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut app.chat_input)
                                .hint_text("Введите сообщение...")
                                .desired_width(ui.available_width() - 90.0)
                                .font(egui::FontId::proportional(14.0))
                                .text_color(CLR_TEXT),
                        );
                        let send_clicked = ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Отправить").strong().color(egui::Color32::WHITE),
                                )
                                .fill(CLR_ACCENT),
                            )
                            .clicked();

                        if (send_clicked
                            || (resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter))))
                            && !app.chat_input.trim().is_empty()
                        {
                            let txt = app.chat_input.clone();
                            app.chat_input.clear();
                            app.send_text_message(friend.user_code.clone(), txt);
                        }
                    });
                });

            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(CLR_BG).inner_margin(egui::Margin::same(10.0)))
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            if msgs.is_empty() {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(40.0);
                                    ui.label(
                                        egui::RichText::new("Сообщений пока нет. Напишите первым!")
                                            .color(CLR_TEXT_DIM)
                                            .small(),
                                    );
                                });
                            } else {
                                for m in &msgs {
                                    let is_me = m.from_code == app.cfg.user_code;
                                    let max_bubble_w = (ui.available_width() * 0.75).max(180.0);

                                    let layout = if is_me {
                                        egui::Layout::right_to_left(egui::Align::TOP)
                                    } else {
                                        egui::Layout::left_to_right(egui::Align::TOP)
                                    };

                                    ui.with_layout(layout, |ui| {
                                        egui::Frame::none()
                                            .fill(if is_me {
                                                egui::Color32::from_rgb(30, 75, 120)
                                            } else {
                                                CLR_SURFACE
                                            })
                                            .rounding(egui::Rounding::same(8.0))
                                            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                                            .stroke(egui::Stroke::new(
                                                1.0_f32,
                                                if is_me { CLR_BLUE } else { CLR_BORDER },
                                            ))
                                            .show(ui, |ui| {
                                                ui.set_max_width(max_bubble_w);
                                                ui.vertical(|ui| {
                                                    if !is_me {
                                                        ui.label(
                                                            egui::RichText::new(&m.from_name)
                                                                .size(11.0)
                                                                .color(CLR_ACCENT)
                                                                .strong(),
                                                        );
                                                    }
                                                    ui.add(
                                                        egui::Label::new(
                                                            egui::RichText::new(&m.text)
                                                                .size(14.0)
                                                                .color(CLR_TEXT),
                                                        )
                                                        .wrap(true),
                                                    );
                                                });
                                            });
                                    });
                                    ui.add_space(4.0);
                                }
                            }
                        });
                });
        });

    if !open {
        app.chat_active_friend = None;
    }
}

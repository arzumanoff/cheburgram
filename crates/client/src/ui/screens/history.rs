use crate::app::App;
use crate::ui::theme::*;
use cheburgram_protocol::CallDirection;
use eframe::egui;

enum HistAction {
    Call(String, String),
    Chat(String, String),
    AddFriend(String),
}

pub fn draw_history(app: &mut App, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new("История звонков").size(15.0).strong().color(CLR_TEXT));
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("📞 — перезвонить, 💬 — написать, ➕ — добавить в друзья")
            .small()
            .color(CLR_TEXT_DIM),
    );
    ui.add_space(8.0);

    if app.cfg.call_history.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(egui::RichText::new("История пуста").color(CLR_TEXT_DIM));
        });
        return;
    }

    let history = app.cfg.call_history.clone();
    let friends = app.cfg.friends.clone();
    let statuses = app.friend_statuses.clone();
    let mut action: Option<HistAction> = None;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for r in &history {
            let known_code = !r.peer_code.is_empty();
            let is_friend = friends.iter().any(|f| f.user_code == r.peer_code);
            let is_online = statuses.get(&r.peer_code).map(|s| s.is_online).unwrap_or(false);
            let can_call = known_code && (!is_friend || is_online);

            egui::Frame::none()
                .fill(CLR_SURFACE)
                .rounding(egui::Rounding::same(8.0))
                .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                .stroke(egui::Stroke::new(1.0_f32, CLR_BORDER))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (dir_txt, dir_col) = match r.direction {
                            CallDirection::Incoming => ("Входящий", CLR_GREEN),
                            CallDirection::Outgoing => ("Исходящий", CLR_BLUE),
                            CallDirection::Missed => ("Пропущенный", CLR_RED),
                        };
                        let (strip, _) =
                            ui.allocate_exact_size(egui::vec2(3.0, 36.0), egui::Sense::hover());
                        ui.painter().rect_filled(strip, egui::Rounding::same(2.0), dir_col);
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&r.peer_name).strong().color(CLR_TEXT));
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(dir_txt).small().color(dir_col));
                                let ts = r.timestamp.get(..16).unwrap_or(&r.timestamp).replace('T', " ");
                                let sub = if r.duration_secs > 0 {
                                    format!(
                                        "{:02}:{:02} • {}",
                                        r.duration_secs / 60,
                                        r.duration_secs % 60,
                                        ts
                                    )
                                } else {
                                    ts
                                };
                                ui.label(egui::RichText::new(sub).small().color(CLR_TEXT_DIM));
                            });
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if known_code && !is_friend {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            egui::RichText::new("➕").color(egui::Color32::WHITE).strong(),
                                        )
                                        .fill(CLR_ACCENT)
                                        .min_size(egui::vec2(30.0, 28.0)),
                                    )
                                    .on_hover_text("Добавить в друзья")
                                    .clicked()
                                {
                                    action = Some(HistAction::AddFriend(r.peer_code.clone()));
                                }
                                ui.add_space(4.0);
                            }
                            if ui
                                .add_enabled(
                                    known_code,
                                    egui::Button::new(
                                        egui::RichText::new("💬").color(egui::Color32::WHITE).strong(),
                                    )
                                    .fill(CLR_BLUE)
                                    .min_size(egui::vec2(30.0, 28.0)),
                                )
                                .on_hover_text(if known_code { "Написать" } else { "ID неизвестен (старая запись)" })
                                .clicked()
                            {
                                action = Some(HistAction::Chat(r.peer_code.clone(), r.peer_name.clone()));
                            }
                            ui.add_space(4.0);
                            if ui
                                .add_enabled(
                                    can_call,
                                    egui::Button::new(
                                        egui::RichText::new("📞").color(egui::Color32::WHITE).strong(),
                                    )
                                    .fill(if is_online { CLR_GREEN } else { CLR_SURFACE2 })
                                    .min_size(egui::vec2(30.0, 28.0)),
                                )
                                .on_hover_text(if can_call {
                                    "Перезвонить"
                                } else if !known_code {
                                    "ID неизвестен (старая запись)"
                                } else {
                                    "Собеседник не в сети"
                                })
                                .clicked()
                            {
                                action = Some(HistAction::Call(r.peer_code.clone(), r.peer_name.clone()));
                            }
                        });
                    });
                });
            ui.add_space(5.0);
        }
    });

    match action {
        Some(HistAction::Call(code, name)) => app.call_user(code, name),
        Some(HistAction::Chat(code, name)) => app.open_chat(code, name),
        Some(HistAction::AddFriend(code)) => app.send_friend_request(&code),
        None => {}
    }
}

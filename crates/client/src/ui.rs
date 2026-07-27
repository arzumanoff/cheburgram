//! UI-слой (egui). Визуал v2 с исправленной проводкой:
//! мьюты/громкость идут через атомики аудиодвижка, статусы — из честного
//! состояния сети. Полный редизайн по дизайн-системе — этап E3.

use eframe::egui;
use std::time::Duration;

use crate::app::{App, CallState, Tab};
use crate::config::save_config;
use cheburgram_protocol::CallDirection;

// ─── Палитра ─────────────────────────────────────────────────────────────────

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

pub fn apply_theme(ctx: &egui::Context) {
    let mut vis = egui::Visuals::dark();
    vis.panel_fill = CLR_BG;
    vis.window_fill = CLR_SURFACE;
    vis.window_rounding = egui::Rounding::same(10.0);
    vis.widgets.noninteractive.bg_fill = CLR_SURFACE;
    vis.widgets.inactive.bg_fill = CLR_SURFACE2;
    vis.widgets.hovered.bg_fill = egui::Color32::from_rgb(40, 48, 60);
    vis.widgets.active.bg_fill = egui::Color32::from_rgb(50, 60, 75);
    vis.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, CLR_TEXT_DIM);
    vis.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, CLR_TEXT);
    vis.extreme_bg_color = egui::Color32::from_rgb(10, 14, 20);
    ctx.set_visuals(vis);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}

// ─── Корневой кадр ────────────────────────────────────────────────────────────

pub fn draw_root(app: &mut App, ctx: &egui::Context) {
    app.poll();

    if ctx.input(|i| i.viewport().close_requested()) {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        app.show_close_dialog = true;
    }

    if app.show_close_dialog {
        draw_close_dialog(app, ctx);
    }

    let zoom = if app.cfg.zoom_factor < 0.5 { 1.0 } else { app.cfg.zoom_factor };
    ctx.set_pixels_per_point(zoom);
    apply_theme(ctx);

    // экран входа
    if !app.is_connected && app.cfg.display_name.is_empty() {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(CLR_BG)
                    .inner_margin(egui::Margin::symmetric(16.0, 0.0)),
            )
            .show(ctx, |ui| draw_login(app, ui, ctx));
        ctx.request_repaint_after(Duration::from_millis(50));
        return;
    }

    // автоподключение при первом запуске с сохранённым именем
    app.ensure_net();

    match app.call_state.clone() {
        CallState::Calling { target_name, .. } => {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(CLR_BG)
                        .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                )
                .show(ctx, |ui| draw_calling(app, ui, ctx, &target_name));
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        CallState::IncomingCall { from_name, from_peer_id, .. } => {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(CLR_BG)
                        .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                )
                .show(ctx, |ui| draw_incoming(app, ui, ctx, from_peer_id, &from_name));
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        CallState::InCall { peer_name, started_at, .. } => {
            egui::CentralPanel::default()
                .frame(
                    egui::Frame::none()
                        .fill(CLR_BG)
                        .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
                )
                .show(ctx, |ui| draw_in_call(app, ui, &peer_name, started_at));
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }
        CallState::None => {}
    }

    egui::TopBottomPanel::top("topbar")
        .frame(egui::Frame::none().fill(CLR_SURFACE).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                draw_chebu_icon(ui, 26.0);
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Cheburgram").size(15.0).strong().color(CLR_ACCENT));
                ui.add_space(14.0);

                let tab_btn = |ui: &mut egui::Ui, current: Tab, target: Tab, text: &str| -> bool {
                    let is_sel = current == target;
                    let bg = if is_sel { CLR_SURFACE2 } else { egui::Color32::TRANSPARENT };
                    let fg = if is_sel { CLR_ACCENT } else { CLR_TEXT_DIM };
                    ui.add(
                        egui::Button::new(egui::RichText::new(text).size(13.0).color(fg).strong())
                            .fill(bg)
                            .min_size(egui::vec2(108.0, 28.0))
                            .rounding(egui::Rounding::same(6.0)),
                    )
                    .clicked()
                };

                if tab_btn(ui, app.active_tab, Tab::Contacts, "👥 Друзья") {
                    app.active_tab = Tab::Contacts;
                    app.request_friends_status();
                }
                if tab_btn(ui, app.active_tab, Tab::History, "📋 История") {
                    app.active_tab = Tab::History;
                }
                if tab_btn(ui, app.active_tab, Tab::Settings, "⚙ Настройки") {
                    app.active_tab = Tab::Settings;
                }
            });
        });

    egui::TopBottomPanel::bottom("statusbar")
        .frame(egui::Frame::none().fill(CLR_SURFACE).inner_margin(egui::Margin::symmetric(12.0, 4.0)))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let dot_col = if app.is_connected {
                    CLR_GREEN
                } else if app.link_up {
                    CLR_YELLOW
                } else {
                    CLR_RED
                };
                let (dot_rect, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot_rect.center(), 4.0, dot_col);
                ui.label(egui::RichText::new(&app.status).small().color(CLR_TEXT_DIM));
            });
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::none()
                .fill(CLR_BG)
                .inner_margin(egui::Margin::symmetric(14.0, 4.0)),
        )
        .show(ctx, |ui| match app.active_tab {
            Tab::Contacts => draw_contacts(app, ui, ctx),
            Tab::History => draw_history(app, ui),
            Tab::Settings => draw_settings(app, ui),
        });

    draw_chat_modal(app, ctx);

    ctx.request_repaint_after(Duration::from_millis(50));
}

fn draw_close_dialog(app: &mut App, ctx: &egui::Context) {
    egui::Window::new("Закрыть Cheburgram?")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(CLR_SURFACE)
                .stroke(egui::Stroke::new(1.0_f32, CLR_BORDER))
                .rounding(egui::Rounding::same(12.0)),
        )
        .show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Что вы хотите сделать?").color(CLR_TEXT));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "При сворачивании в трей программа продолжает работать\nи вы сможете принимать звонки.",
                )
                .small()
                .color(CLR_TEXT_DIM),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Свернуть в трей").color(egui::Color32::WHITE).strong(),
                        )
                        .fill(CLR_ACCENT)
                        .min_size(egui::vec2(140.0, 36.0)),
                    )
                    .clicked()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    app.show_close_dialog = false;
                }
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Выйти").color(egui::Color32::WHITE).strong(),
                        )
                        .fill(CLR_RED)
                        .min_size(egui::vec2(80.0, 36.0)),
                    )
                    .clicked()
                {
                    std::process::exit(0);
                }
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Отмена").color(CLR_TEXT))
                            .fill(CLR_SURFACE2)
                            .min_size(egui::vec2(80.0, 36.0)),
                    )
                    .clicked()
                {
                    app.show_close_dialog = false;
                }
            });
            ui.add_space(4.0);
        });
}

// ─── Экран входа ──────────────────────────────────────────────────────────────

fn draw_login(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                                .hint_text("ip:7878")
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

// ─── Контакты ────────────────────────────────────────────────────────────────

/// Оставить в строке только цифры, не более 6 (ловит и вставку из буфера)
fn sanitize_id_input(s: &mut String) {
    let filtered: String = s.chars().filter(|c| c.is_ascii_digit()).take(6).collect();
    if filtered != *s {
        *s = filtered;
    }
}

fn draw_contacts(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                    // только цифры (паста из буфера с мусором очистится сама)
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

// ─── Чат (модальное окно; станет панелью на этапе E3) ────────────────────────

fn draw_chat_modal(app: &mut App, ctx: &egui::Context) {
    let friend = match app.chat_active_friend.clone() {
        Some(f) => f,
        None => return,
    };

    let mut open = true;
    egui::Window::new(format!("💬 Чат с {} (ID: {})", friend.name, friend.user_code))
        .open(&mut open)
        .resizable(true)
        .collapsible(false)
        .default_size(egui::vec2(420.0, 500.0))
        .min_size(egui::vec2(320.0, 380.0))
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
                        // не-другу можно звонить «вслепую» — сервер ответит статусом
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
                                    ui.horizontal(|ui| {
                                        if is_me {
                                            let space_w = (ui.available_width() - 260.0).max(0.0);
                                            ui.add_space(space_w);
                                        }
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
                                                ui.set_max_width(240.0);
                                                ui.vertical(|ui| {
                                                    if !is_me {
                                                        ui.label(
                                                            egui::RichText::new(&m.from_name)
                                                                .size(11.0)
                                                                .color(CLR_ACCENT)
                                                                .strong(),
                                                        );
                                                    }
                                                    ui.label(
                                                        egui::RichText::new(&m.text).size(14.0).color(CLR_TEXT),
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

// ─── Звонки ──────────────────────────────────────────────────────────────────

fn draw_calling(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context, to_name: &str) {
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

fn draw_incoming(app: &mut App, ui: &mut egui::Ui, ctx: &egui::Context, from_peer_id: u32, from_name: &str) {
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

fn draw_in_call(app: &mut App, ui: &mut egui::Ui, peer_name: &str, started_at: std::time::Instant) {
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
                ui.label(
                    egui::RichText::new(format!("{:02}:{:02}", el / 60, el % 60))
                        .size(13.0)
                        .color(CLR_TEXT_DIM),
                );

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

// ─── История ─────────────────────────────────────────────────────────────────

/// Действие из строки истории
enum HistAction {
    Call(String, String),
    Chat(String, String),
    AddFriend(String),
}

fn draw_history(app: &mut App, ui: &mut egui::Ui) {
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
            // другу — только если онлайн; не-другу (статус неизвестен) — даём попробовать
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

// ─── Настройки ────────────────────────────────────────────────────────────────

fn draw_settings(app: &mut App, ui: &mut egui::Ui) {
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

// ─── Иконка и утилиты ─────────────────────────────────────────────────────────

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

fn draw_chebu_icon(ui: &mut egui::Ui, size: f32) {
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

fn draw_chebu_icon_large(ui: &mut egui::Ui, size: f32) {
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

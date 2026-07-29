pub mod screens;
pub mod theme;
pub mod widgets;

use eframe::egui;
use std::time::Duration;

use crate::app::{App, CallState, Tab};
pub use theme::{
    apply_theme, CLR_ACCENT, CLR_BG, CLR_BORDER, CLR_GREEN, CLR_RED, CLR_SURFACE,
    CLR_SURFACE2, CLR_TEXT, CLR_TEXT_DIM, CLR_YELLOW,
};
use screens::chat::draw_chat_modal;
use screens::contacts::draw_contacts;
use screens::history::draw_history;
use screens::login::draw_login;
use screens::settings::draw_settings;
use widgets::call_overlay::{draw_calling, draw_in_call, draw_incoming};
use widgets::icons::draw_chebu_icon;

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
    apply_theme(ctx, app.cfg.dark_mode);

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

                if app.cfg.tls_enabled {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new("🔒 TLS Encrypted").small().color(CLR_GREEN));
                    });
                }
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

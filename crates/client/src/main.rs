#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod net;
mod ui;

use anyhow::{anyhow, Result};
use eframe::egui;
use std::fs;
#[cfg(target_os = "windows")]
use std::{thread, time::Duration};
use tracing::info;
#[cfg(target_os = "windows")]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use app::App;

struct CheburgramApp {
    app: App,
    #[cfg(target_os = "windows")]
    _tray_icon: Option<TrayIcon>,
}

impl eframe::App for CheburgramApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ui::draw_root(&mut self.app, ctx);
    }
}

// ─── Трей (Windows) ──────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn create_tray_icon() -> Option<(TrayIcon, tray_icon::menu::MenuId, tray_icon::menu::MenuId)> {
    let rgba = make_icon_rgba();
    let icon = tray_icon::Icon::from_rgba(rgba, 32, 32).ok()?;

    let open = MenuItem::new("Открыть Cheburgram", true, None);
    let quit = MenuItem::new("Выйти", true, None);
    let open_id = open.id().clone();
    let quit_id = quit.id().clone();

    let menu = Menu::new();
    let _ = menu.append(&open);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&quit);

    let tray = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Cheburgram — голосовой мессенджер")
        .with_menu(Box::new(menu))
        .build()
        .ok()?;

    Some((tray, open_id, quit_id))
}

/// Автозагрузка — ТОЛЬКО по явному включению в конфиге (в v2 прописывалась молча)
#[cfg(target_os = "windows")]
#[allow(dead_code)]
fn sync_autostart(enabled: bool) {
    use winreg::enums::*;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok((run, _)) = hkcu.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run") {
        if enabled {
            if let Ok(exe) = std::env::current_exe() {
                let _ = run.set_value("Cheburgram", &exe.to_string_lossy().to_string());
            }
        } else {
            let _ = run.delete_value("Cheburgram");
        }
    }
}

// ─── Шрифты и иконка ─────────────────────────────────────────────────────────

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in &["C:\\Windows\\Fonts\\segoeui.ttf", "C:\\Windows\\Fonts\\seguiemj.ttf"] {
        if let Ok(data) = fs::read(path) {
            let name = path.split('\\').last().unwrap_or("font").to_string();
            fonts.font_data.insert(name.clone(), egui::FontData::from_owned(data));
            for fam in [&egui::FontFamily::Proportional, &egui::FontFamily::Monospace] {
                if let Some(v) = fonts.families.get_mut(fam) {
                    v.push(name.clone());
                }
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn make_icon_rgba() -> Vec<u8> {
    let size = 32usize;
    let mut rgba = vec![0u8; size * size * 4];
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let r_bg = size as f32 / 2.0 - 0.5;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let i = (y * size + x) * 4;
            if dist > r_bg {
                rgba[i + 3] = 0;
                continue;
            }
            rgba[i] = 140;
            rgba[i + 1] = 195;
            rgba[i + 2] = 230;
            rgba[i + 3] = 255;
            if (dx - (-11.0)).hypot(dy - (-1.0)) < 5.5 || (dx - 11.0).hypot(dy - (-1.0)) < 5.5 {
                rgba[i] = 166;
                rgba[i + 1] = 110;
                rgba[i + 2] = 71;
            }
            if dx.hypot(dy - 1.0) < 9.5 {
                rgba[i] = 166;
                rgba[i + 1] = 110;
                rgba[i + 2] = 71;
            }
            if dx.hypot(dy - 2.0) < 7.0 {
                rgba[i] = 245;
                rgba[i + 1] = 214;
                rgba[i + 2] = 184;
            }
            if (dx + 2.8).hypot(dy - 0.5) < 1.8 || (dx - 2.8).hypot(dy - 0.5) < 1.8 {
                rgba[i] = 40;
                rgba[i + 1] = 20;
                rgba[i + 2] = 10;
            }
        }
    }
    rgba
}

fn make_app_icon() -> egui::IconData {
    let rgba = make_icon_rgba();
    egui::IconData { rgba, width: 32, height: 32 }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Cheburgram v3.0 запуск...");

    let icon = make_app_icon();
    let vp = egui::ViewportBuilder::default()
        .with_inner_size([430.0, 600.0])
        .with_min_inner_size([380.0, 500.0])
        .with_title("Cheburgram")
        .with_resizable(true)
        .with_icon(icon);

    let options = eframe::NativeOptions { viewport: vp, ..Default::default() };

    eframe::run_native(
        "Cheburgram",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);

            #[allow(unused_mut)]
            let mut app = App::new();

            #[cfg(target_os = "windows")]
            let tray_icon = {
                let (tray_icon, open_id, quit_id) = create_tray_icon()
                    .map(|(t, o, q)| (Some(t), Some(o), Some(q)))
                    .unwrap_or((None, None, None));
                app.tray_open_id = open_id.clone();
                app.tray_quit_id = quit_id.clone();

                let egui_ctx = cc.egui_ctx.clone();
                thread::spawn(move || {
                    let menu_rx = MenuEvent::receiver();
                    let tray_rx = TrayIconEvent::receiver();
                    loop {
                        let mut wake = false;
                        while let Ok(ev) = menu_rx.try_recv() {
                            if quit_id.as_ref().map(|id| *id == ev.id).unwrap_or(false) {
                                std::process::exit(0);
                            }
                            if open_id.as_ref().map(|id| *id == ev.id).unwrap_or(false) {
                                wake = true;
                            }
                        }
                        while let Ok(ev) = tray_rx.try_recv() {
                            match ev {
                                TrayIconEvent::Click { .. } => {
                                    wake = true;
                                }
                                _ => {}
                            }
                        }
                        if wake {
                            egui_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                            egui_ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                            egui_ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                            egui_ctx.request_repaint();
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                });
                tray_icon
            };

            Box::new(CheburgramApp {
                app,
                #[cfg(target_os = "windows")]
                _tray_icon: tray_icon,
            })
        }),
    )
    .map_err(|e| anyhow!("GUI: {}", e))?;

    Ok(())
}

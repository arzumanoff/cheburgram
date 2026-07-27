#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{anyhow, Result};
use cheburgram_protocol::{
    AudioPacket, CallDirection, CallRecord, ControlMessage, FriendStatus,
};
use chrono::Utc;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use opus::{Application, Channels, Decoder, Encoder};
use ringbuf::HeapRb;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream as StdTcpStream, UdpSocket as StdUdpSocket},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        mpsc::{channel, Receiver},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tracing::{error, info};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use uuid::Uuid;

const SAMPLE_RATE: u32 = 48000;
const FRAME_SIZE: usize = 960;
const DEFAULT_SERVER: &str = "85.192.25.57:7878";

// Цветовая палитра
const CLR_BG: egui::Color32 = egui::Color32::from_rgb(13, 17, 23);
const CLR_SURFACE: egui::Color32 = egui::Color32::from_rgb(22, 27, 34);
const CLR_SURFACE2: egui::Color32 = egui::Color32::from_rgb(30, 37, 46);
const CLR_ACCENT: egui::Color32 = egui::Color32::from_rgb(255, 140, 0);
const CLR_GREEN: egui::Color32 = egui::Color32::from_rgb(35, 197, 94);
const CLR_RED: egui::Color32 = egui::Color32::from_rgb(218, 54, 51);
const CLR_BLUE: egui::Color32 = egui::Color32::from_rgb(88, 166, 255);
const CLR_TEXT: egui::Color32 = egui::Color32::from_rgb(230, 237, 243);
const CLR_TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(139, 148, 158);
const CLR_BORDER: egui::Color32 = egui::Color32::from_rgb(48, 54, 61);

// ─── Сохранённый друг ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct SavedFriend {
    user_code: String,
    name: String,
}

// ─── Конфигурация (%APPDATA%\Cheburgram\config.json) ──────────────────────────

#[derive(Serialize, Deserialize, Clone)]
struct AppConfig {
    client_id: String,
    user_code: String,
    display_name: String,
    server_address: String,
    selected_input: usize,
    selected_output: usize,
    friends: Vec<SavedFriend>,
    call_history: Vec<CallRecord>,
}

impl Default for AppConfig {
    fn default() -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let code = format!("{:06}", rng.gen_range(100_000..999_999));
        Self {
            client_id: Uuid::new_v4().to_string(),
            user_code: code,
            display_name: String::new(),
            server_address: DEFAULT_SERVER.to_string(),
            selected_input: 0,
            selected_output: 0,
            friends: Vec::new(),
            call_history: Vec::new(),
        }
    }
}

fn config_path() -> PathBuf {
    let base = if let Ok(ap) = std::env::var("APPDATA") {
        PathBuf::from(ap).join("Cheburgram")
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
    };
    fs::create_dir_all(&base).ok();
    base.join("config.json")
}

fn load_config() -> AppConfig {
    let p = config_path();
    if p.exists() {
        if let Ok(d) = fs::read_to_string(&p) {
            if let Ok(c) = serde_json::from_str::<AppConfig>(&d) {
                return c;
            }
        }
    }
    AppConfig::default()
}

fn save_config(c: &AppConfig) {
    if let Ok(d) = serde_json::to_string_pretty(c) {
        let _ = fs::write(config_path(), d);
    }
}

// ─── Вкладки / Экраны ─────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy, Debug)]
enum Tab {
    Contacts,
    History,
    Settings,
}

#[derive(PartialEq, Clone, Debug)]
enum CallState {
    None,
    Calling { target_code: String, target_name: String },
    IncomingCall { from_code: String, from_name: String, from_peer_id: u32 },
    InCall { peer_id: u32, peer_name: String, call_id: u64, started_at: Instant },
}

// ─── Аудио устройства ────────────────────────────────────────────────────────

struct AudioDevs {
    inputs: Vec<String>,
    outputs: Vec<String>,
    sel_in: usize,
    sel_out: usize,
}

fn list_audio_devs() -> AudioDevs {
    let host = cpal::default_host();
    let mut ins = vec!["По умолчанию".to_string()];
    let mut outs = vec!["По умолчанию".to_string()];
    if let Ok(d) = host.input_devices() {
        for dev in d { if let Ok(n) = dev.name() { ins.push(n); } }
    }
    if let Ok(d) = host.output_devices() {
        for dev in d { if let Ok(n) = dev.name() { outs.push(n); } }
    }
    AudioDevs { inputs: ins, outputs: outs, sel_in: 0, sel_out: 0 }
}

// ─── Главное приложение ───────────────────────────────────────────────────────

struct App {
    cfg: AppConfig,
    active_tab: Tab,
    call_state: CallState,
    status: String,
    is_connected: bool,

    name_input: String,
    add_friend_input: String,
    copied_code_banner: bool,

    // Онлайн статус сохранённых друзей (user_code -> FriendStatus)
    friend_statuses: std::collections::HashMap<String, FriendStatus>,
    my_peer_id: Option<u32>,

    event_rx: Option<Receiver<ControlMessage>>,
    stop: Arc<AtomicBool>,
    tcp_writer: Option<Arc<Mutex<StdTcpStream>>>,

    devs: AudioDevs,
    mic_level: Arc<AtomicU8>,
    mic_muted: bool,
    snd_muted: bool,
    pkts_sent: Arc<AtomicU64>,
    pkts_recv: Arc<AtomicU64>,
    udp_sock: Option<Arc<StdUdpSocket>>,
    call_id_a: Arc<AtomicU64>,
    call_start: Option<Instant>,

    // Тест микрофона
    mic_test_level: Arc<AtomicU8>,
    mic_test_stop: Arc<AtomicBool>,
    mic_test_active: bool,

    // Трей
    show_close_dialog: bool,
    _tray_icon: Option<TrayIcon>,
    tray_open_id: Option<tray_icon::menu::MenuId>,
    tray_quit_id: Option<tray_icon::menu::MenuId>,
}

impl Default for App {
    fn default() -> Self {
        let cfg = load_config();
        let name_input = cfg.display_name.clone();
        let mut devs = list_audio_devs();
        devs.sel_in = cfg.selected_input.min(devs.inputs.len().saturating_sub(1));
        devs.sel_out = cfg.selected_output.min(devs.outputs.len().saturating_sub(1));
        let (tray_icon, open_id, quit_id) = create_tray_icon().map(|(t, o, q)| (Some(t), Some(o), Some(q))).unwrap_or((None, None, None));

        Self {
            cfg,
            active_tab: Tab::Contacts,
            call_state: CallState::None,
            status: String::new(),
            is_connected: false,
            name_input,
            add_friend_input: String::new(),
            copied_code_banner: false,
            friend_statuses: std::collections::HashMap::new(),
            my_peer_id: None,
            event_rx: None,
            stop: Arc::new(AtomicBool::new(false)),
            tcp_writer: None,
            devs,
            mic_level: Arc::new(AtomicU8::new(0)),
            mic_muted: false,
            snd_muted: false,
            pkts_sent: Arc::new(AtomicU64::new(0)),
            pkts_recv: Arc::new(AtomicU64::new(0)),
            udp_sock: None,
            call_id_a: Arc::new(AtomicU64::new(0)),
            call_start: None,
            mic_test_level: Arc::new(AtomicU8::new(0)),
            mic_test_stop: Arc::new(AtomicBool::new(false)),
            mic_test_active: false,
            show_close_dialog: false,
            _tray_icon: tray_icon,
            tray_open_id: open_id,
            tray_quit_id: quit_id,
        }
    }
}

impl App {
    /// Отправка команды серверу строго через ЕДИНЫЙ активный TCP поток
    fn send_msg(&self, msg: ControlMessage) {
        if let Some(w) = &self.tcp_writer {
            if let Ok(mut stream) = w.lock() {
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = stream.write_all(format!("{}\n", json).as_bytes());
                }
            }
        }
    }

    fn connect_register(&mut self) {
        let name = self.name_input.trim().to_string();
        if name.is_empty() {
            self.status = "Введите имя!".into();
            return;
        }
        self.cfg.display_name = name.clone();
        save_config(&self.cfg);

        let addr = normalize(&self.cfg.server_address);
        self.status = format!("Подключение к {}...", addr);
        self.stop.store(false, Ordering::SeqCst);

        let (tx, rx) = channel::<ControlMessage>();
        self.event_rx = Some(rx);

        let client_id = self.cfg.client_id.clone();
        let user_code = self.cfg.user_code.clone();
        let stop = self.stop.clone();

        let stream = match StdTcpStream::connect(&addr) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Ошибка подключения: {}", e);
                self.is_connected = false;
                return;
            }
        };

        let writer = Arc::new(Mutex::new(stream.try_clone().unwrap()));
        self.tcp_writer = Some(writer.clone());

        // Отправляем Register
        {
            let mut w = writer.lock().unwrap();
            let msg = ControlMessage::Register {
                client_id,
                user_code,
                name,
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = w.write_all(format!("{}\n", json).as_bytes());
            }
        }

        // Читаем ответы в фоне
        thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            while !stop.load(Ordering::Relaxed) {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(ControlMessage::Error {
                            message: "Соединение с сервером разорвано".into(),
                        });
                        break;
                    }
                    Ok(_) => {
                        if let Ok(msg) = serde_json::from_str::<ControlMessage>(&line) {
                            if tx.send(msg).is_err() { break; }
                        }
                    }
                }
            }
        });
    }

    fn request_friends_status(&self) {
        let codes: Vec<String> = self.cfg.friends.iter().map(|f| f.user_code.clone()).collect();
        if !codes.is_empty() {
            self.send_msg(ControlMessage::GetFriendsStatus { user_codes: codes });
        }
    }

    fn add_friend_by_code(&mut self, code: &str) {
        let clean = code.trim().to_string();
        if clean.len() != 6 || !clean.chars().all(|c| c.is_ascii_digit()) {
            self.status = "ID должен состоять из 6 цифр".into();
            return;
        }
        if clean == self.cfg.user_code {
            self.status = "Нельзя добавить свой собственный ID".into();
            return;
        }
        if self.cfg.friends.iter().any(|f| f.user_code == clean) {
            self.status = "Этот контакт уже у вас в друзьях".into();
            return;
        }

        self.status = format!("Поиск ID {}...", clean);
        self.send_msg(ControlMessage::LookupUser { user_code: clean });
    }

    fn call_user(&mut self, target_code: String, target_name: String) {
        self.call_start = Some(Instant::now());
        self.call_state = CallState::Calling {
            target_code: target_code.clone(),
            target_name: target_name.clone(),
        };
        self.status = format!("Вызов {}...", target_name);
        self.send_msg(ControlMessage::CallRequest { target_code });
    }

    fn accept_call(&mut self, from_peer_id: u32, from_name: String) {
        self.call_start = Some(Instant::now());
        self.send_msg(ControlMessage::CallAccept { target_peer_id: from_peer_id });
        self.status = format!("Соединение с {}...", from_name);
    }

    fn reject_call(&mut self, from_peer_id: u32, from_name: &str) {
        self.send_msg(ControlMessage::CallReject { target_peer_id: from_peer_id });
        self.cfg.call_history.insert(0, CallRecord {
            peer_name: from_name.to_string(),
            direction: CallDirection::Missed,
            timestamp: Utc::now().to_rfc3339(),
            duration_secs: 0,
        });
        save_config(&self.cfg);
        self.call_state = CallState::None;
        self.status = "Звонок отклонён".into();
    }

    fn end_call(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.udp_sock = None;

        let (peer_name, duration) = match &self.call_state {
            CallState::InCall { peer_name, started_at, .. } => {
                (peer_name.clone(), started_at.elapsed().as_secs())
            }
            CallState::Calling { target_name, .. } => (target_name.clone(), 0),
            _ => { self.call_state = CallState::None; return; }
        };

        self.cfg.call_history.insert(0, CallRecord {
            peer_name,
            direction: CallDirection::Outgoing,
            timestamp: Utc::now().to_rfc3339(),
            duration_secs: duration,
        });
        if self.cfg.call_history.len() > 50 { self.cfg.call_history.truncate(50); }
        save_config(&self.cfg);
        self.send_msg(ControlMessage::CallEnd);
        self.call_state = CallState::None;
        self.status = "Звонок завершён".into();
        self.mic_level.store(0, Ordering::Relaxed);
        self.pkts_sent.store(0, Ordering::Relaxed);
        self.pkts_recv.store(0, Ordering::Relaxed);
    }

    fn start_audio(&mut self, peer_id: u32, udp_port: u16, call_id: u64) {
        let server_ip = self.cfg.server_address
            .split(':').next().unwrap_or("85.192.25.57").to_string();

        let sock = match StdUdpSocket::bind("0.0.0.0:0") {
            Ok(s) => Arc::new(s),
            Err(e) => { self.status = format!("UDP ошибка: {}", e); return; }
        };

        let target: SocketAddr = format!("{}:{}", server_ip, udp_port)
            .parse().unwrap_or_else(|_| "85.192.25.57:7879".parse().unwrap());

        self.udp_sock = Some(sock.clone());
        self.call_id_a.store(call_id, Ordering::Relaxed);
        self.stop.store(false, Ordering::SeqCst);

        let my_id = self.my_peer_id.unwrap_or(0);

        // UDP keepalive
        {
            let s = sock.clone();
            let st = self.stop.clone();
            thread::spawn(move || {
                while !st.load(Ordering::Relaxed) {
                    let p = AudioPacket::new(call_id, my_id, 0, vec![]);
                    if let Ok(b) = p.to_bytes() { let _ = s.send_to(&b, target); }
                    thread::sleep(Duration::from_secs(2));
                }
            });
        }

        // Микрофон
        let in_dev = self.devs.inputs.get(self.devs.sel_in).cloned().unwrap_or_default();
        {
            let s = sock.clone();
            let st = self.stop.clone();
            let lv = self.mic_level.clone();
            let sn = self.pkts_sent.clone();
            let m = self.mic_muted;
            thread::spawn(move || {
                if let Err(e) = audio_in(in_dev, s, target, call_id, my_id, st, lv, sn, m) {
                    error!("Микрофон: {:?}", e);
                }
            });
        }

        // Вывод
        let out_dev = self.devs.outputs.get(self.devs.sel_out).cloned().unwrap_or_default();
        {
            let s = sock.clone();
            let st = self.stop.clone();
            let rc = self.pkts_recv.clone();
            let ca = self.call_id_a.clone();
            thread::spawn(move || {
                if let Err(e) = audio_out(out_dev, s, st, rc, ca) {
                    error!("Вывод: {:?}", e);
                }
            });
        }
    }

    fn start_mic_test(&mut self) {
        self.mic_test_stop.store(false, Ordering::SeqCst);
        self.mic_test_active = true;
        let dev_name = self.devs.inputs.get(self.devs.sel_in).cloned().unwrap_or_default();
        let level = self.mic_test_level.clone();
        let stop = self.mic_test_stop.clone();
        thread::spawn(move || {
            let host = cpal::default_host();
            let device = if !dev_name.is_empty() && dev_name != "По умолчанию" {
                host.input_devices().ok()
                    .and_then(|mut d| d.find(|dev| dev.name().ok().as_deref() == Some(&dev_name)))
                    .or_else(|| host.default_input_device())
            } else {
                host.default_input_device()
            };
            let device = match device { Some(d) => d, None => return };
            let cfg_d = match device.default_input_config() { Ok(c) => c, Err(_) => return };
            let fmt = cfg_d.sample_format();
            let config: cpal::StreamConfig = cfg_d.into();
            let ch = config.channels as usize;
            let buf = Arc::new(Mutex::new(Vec::<f32>::new()));
            let buf_c = buf.clone();
            let err_fn = |e: cpal::StreamError| error!("mic test: {}", e);
            let stream = match fmt {
                cpal::SampleFormat::F32 => device.build_input_stream(&config,
                    move |data: &[f32], _: &_| {
                        let mut b = buf_c.lock().unwrap();
                        if ch == 1 { b.extend_from_slice(data); }
                        else { for chunk in data.chunks(ch) { b.push(chunk.iter().sum::<f32>() / ch as f32); } }
                    }, err_fn, None),
                cpal::SampleFormat::I16 => device.build_input_stream(&config,
                    move |data: &[i16], _: &_| {
                        let mut b = buf_c.lock().unwrap();
                        if ch == 1 { b.extend(data.iter().map(|&s| s as f32 / 32768.0)); }
                        else { for chunk in data.chunks(ch) { b.push(chunk.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32); } }
                    }, err_fn, None),
                _ => return,
            };
            let stream = match stream { Ok(s) => s, Err(_) => return };
            let _ = stream.play();
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(30));
                let samples: Vec<f32> = { let mut b = buf.lock().unwrap(); std::mem::take(&mut *b) };
                if !samples.is_empty() {
                    let rms = (samples.iter().map(|s| s*s).sum::<f32>() / samples.len() as f32).sqrt();
                    level.store(((rms * 20.0).sqrt() * 100.0).min(100.0) as u8, Ordering::Relaxed);
                }
            }
            level.store(0, Ordering::Relaxed);
        });
    }

    fn stop_mic_test(&mut self) {
        self.mic_test_stop.store(true, Ordering::SeqCst);
        self.mic_test_active = false;
    }

    fn poll(&mut self) {
        let mut evs = Vec::new();
        if let Some(rx) = &self.event_rx {
            while let Ok(m) = rx.try_recv() { evs.push(m); }
        }
        for msg in evs {
            match msg {
                ControlMessage::Registered { peer_id, user_code, udp_port: _ } => {
                    self.my_peer_id = Some(peer_id);
                    self.cfg.user_code = user_code.clone();
                    save_config(&self.cfg);
                    self.is_connected = true;
                    self.status = format!("В сети как {} (ID: {})", self.cfg.display_name, user_code);
                    self.request_friends_status();
                }
                ControlMessage::UserLookupResult { found, user_code, name, is_online, peer_id } => {
                    if found {
                        let friend = SavedFriend { user_code: user_code.clone(), name: name.clone() };
                        if !self.cfg.friends.contains(&friend) {
                            self.cfg.friends.push(friend);
                            save_config(&self.cfg);
                        }
                        self.friend_statuses.insert(user_code.clone(), FriendStatus {
                            user_code: user_code.clone(),
                            name,
                            is_online,
                            peer_id,
                        });
                        self.add_friend_input.clear();
                        self.status = format!("Контакт ID {} добавлен!", user_code);
                    } else {
                        self.status = format!("Пользователь с ID {} не найден", user_code);
                    }
                }
                ControlMessage::FriendsStatus { friends } => {
                    for f in friends {
                        self.friend_statuses.insert(f.user_code.clone(), f);
                    }
                }
                ControlMessage::UserStatusChanged { user_code, is_online, peer_id } => {
                    if let Some(f) = self.friend_statuses.get_mut(&user_code) {
                        f.is_online = is_online;
                        f.peer_id = peer_id;
                    } else if self.cfg.friends.iter().any(|friend| friend.user_code == user_code) {
                        self.request_friends_status();
                    }
                }
                ControlMessage::IncomingCall { from_code, from_name, from_peer_id } => {
                    self.call_state = CallState::IncomingCall {
                        from_code,
                        from_name,
                        from_peer_id,
                    };
                }
                ControlMessage::CallAccepted { peer_id, peer_name } => {
                    let call_id = time_micros();
                    self.start_audio(peer_id, 7879, call_id);
                    let started = self.call_start.unwrap_or_else(Instant::now);
                    self.call_state = CallState::InCall {
                        peer_id,
                        peer_name: peer_name.clone(),
                        call_id,
                        started_at: started,
                    };
                    self.status = format!("В разговоре с {}", peer_name);
                }
                ControlMessage::CallRejected { peer_name, .. } => {
                    self.call_state = CallState::None;
                    self.status = format!("{} недоступен(на)", peer_name);
                    self.cfg.call_history.insert(0, CallRecord {
                        peer_name,
                        direction: CallDirection::Outgoing,
                        timestamp: Utc::now().to_rfc3339(),
                        duration_secs: 0,
                    });
                    save_config(&self.cfg);
                }
                ControlMessage::CallEnded { peer_name } => {
                    self.stop.store(true, Ordering::SeqCst);
                    self.udp_sock = None;
                    let dur = self.call_start.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                    self.cfg.call_history.insert(0, CallRecord {
                        peer_name: peer_name.clone(),
                        direction: CallDirection::Incoming,
                        timestamp: Utc::now().to_rfc3339(),
                        duration_secs: dur,
                    });
                    if self.cfg.call_history.len() > 50 { self.cfg.call_history.truncate(50); }
                    save_config(&self.cfg);
                    self.call_state = CallState::None;
                    self.status = format!("{} завершил(а) звонок", peer_name);
                }
                ControlMessage::Error { message } => {
                    self.status = format!("⚠ {}", message);
                    if matches!(self.call_state, CallState::Calling { .. }) {
                        self.call_state = CallState::None;
                    }
                }
                _ => {}
            }
        }
    }
}

fn time_micros() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_micros() as u64
}

fn normalize(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() { return DEFAULT_SERVER.to_string(); }
    if s.ends_with(":22") { return format!("{}:7878", s.trim_end_matches(":22")); }
    if !s.contains(':') { return format!("{}:7878", s); }
    s.to_string()
}

// ─── Трей и автозагрузка ─────────────────────────────────────────────────────

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

fn setup_autostart() {
    #[cfg(target_os = "windows")]
    {
        use winreg::enums::*;
        use winreg::RegKey;
        if let Ok(exe) = std::env::current_exe() {
            let exe_str = exe.to_string_lossy().to_string();
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            if let Ok(run) = hkcu.open_subkey_with_flags(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                KEY_WRITE,
            ) {
                let _ = run.set_value("Cheburgram", &exe_str);
            } else if let Ok((run, _)) = hkcu.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run") {
                let _ = run.set_value("Cheburgram", &exe_str);
            }
        }
    }
}

// ─── Аудио ───────────────────────────────────────────────────────────────────

fn audio_in(dev: String, sock: Arc<StdUdpSocket>, target: SocketAddr,
    call_id: u64, my_id: u32, stop: Arc<AtomicBool>,
    level: Arc<AtomicU8>, sent: Arc<AtomicU64>, muted: bool) -> Result<()>
{
    let host = cpal::default_host();
    let device = if !dev.is_empty() && dev != "По умолчанию" {
        host.input_devices()?.find(|d| d.name().ok().as_deref() == Some(&dev))
            .ok_or_else(|| anyhow!("Микрофон не найден"))?
    } else {
        host.default_input_device().ok_or_else(|| anyhow!("Нет микрофона"))?
    };
    let dc = device.default_input_config()?;
    let fmt = dc.sample_format();
    let config: cpal::StreamConfig = dc.into();
    let ch = config.channels as usize;
    let mut enc = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
    let buf = Arc::new(Mutex::new(Vec::<f32>::with_capacity(FRAME_SIZE * 4)));
    let bc = buf.clone();
    let ef = |e: cpal::StreamError| error!("cpal in: {}", e);
    let stream = match fmt {
        cpal::SampleFormat::F32 => device.build_input_stream(&config, move |d: &[f32], _: &_| {
            let mut b = bc.lock().unwrap();
            if ch == 1 { b.extend_from_slice(d); }
            else { for c in d.chunks(ch) { b.push(c.iter().sum::<f32>() / ch as f32); } }
        }, ef, None)?,
        cpal::SampleFormat::I16 => device.build_input_stream(&config, move |d: &[i16], _: &_| {
            let mut b = bc.lock().unwrap();
            if ch == 1 { b.extend(d.iter().map(|&s| s as f32 / 32768.0)); }
            else { for c in d.chunks(ch) { b.push(c.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32); } }
        }, ef, None)?,
        _ => return Err(anyhow!("Формат не поддерживается")),
    };
    stream.play()?;
    let mut seq = 1u64;
    let mut obuf = vec![0u8; 4000];
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(10));
        let samples: Vec<f32> = {
            let mut b = buf.lock().unwrap();
            if b.len() < FRAME_SIZE { continue; }
            b.drain(..FRAME_SIZE).collect()
        };
        let rms = (samples.iter().map(|s| s*s).sum::<f32>() / samples.len() as f32).sqrt();
        level.store(((rms * 20.0).sqrt() * 100.0).min(100.0) as u8, Ordering::Relaxed);
        if muted { continue; }
        if let Ok(n) = enc.encode_float(&samples, &mut obuf) {
            let p = AudioPacket::new(call_id, my_id, seq, obuf[..n].to_vec());
            if let Ok(b) = p.to_bytes() { let _ = sock.send_to(&b, target); seq += 1; sent.fetch_add(1, Ordering::Relaxed); }
        }
    }
    Ok(())
}

fn audio_out(dev: String, sock: Arc<StdUdpSocket>, stop: Arc<AtomicBool>,
    recv: Arc<AtomicU64>, call_id_a: Arc<AtomicU64>) -> Result<()>
{
    let host = cpal::default_host();
    let device = if !dev.is_empty() && dev != "По умолчанию" {
        host.output_devices()?.find(|d| d.name().ok().as_deref() == Some(&dev))
            .ok_or_else(|| anyhow!("Динамики не найдены"))?
    } else {
        host.default_output_device().ok_or_else(|| anyhow!("Нет динамиков"))?
    };
    let config = cpal::StreamConfig { channels: 1, sample_rate: cpal::SampleRate(SAMPLE_RATE), buffer_size: cpal::BufferSize::Default };
    let ring = HeapRb::<f32>::new(SAMPLE_RATE as usize);
    let (mut prod, mut cons) = ring.split();
    let stream = device.build_output_stream(&config,
        move |d: &mut [f32], _: &_| { for s in d.iter_mut() { *s = cons.pop().unwrap_or(0.0); } },
        |e: cpal::StreamError| error!("cpal out: {}", e), None)?;
    stream.play()?;
    let mut dec = Decoder::new(SAMPLE_RATE, Channels::Mono)?;
    let mut sbuf = vec![0u8; 65535];
    let mut pcm = vec![0.0f32; FRAME_SIZE * 2];
    sock.set_read_timeout(Some(Duration::from_millis(100)))?;
    while !stop.load(Ordering::Relaxed) {
        if let Ok((n, _)) = sock.recv_from(&mut sbuf) {
            if let Ok(p) = AudioPacket::from_bytes(&sbuf[..n]) {
                if p.payload.is_empty() || p.header.room_id != call_id_a.load(Ordering::Relaxed) { continue; }
                if let Ok(decoded) = dec.decode_float(&p.payload, &mut pcm, false) {
                    for &s in &pcm[..decoded] { let _ = prod.push(s); }
                    recv.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }
    Ok(())
}

// ─── GUI ─────────────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();

        // ── Перехват закрытия окна ─────────────────────────────────────────
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.show_close_dialog = true;
        }

        // ── Диалог закрытия ────────────────────────────────────────────────
        if self.show_close_dialog {
            egui::Window::new("Закрыть Cheburgram?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .frame(egui::Frame::window(&ctx.style())
                    .fill(CLR_SURFACE)
                    .stroke(egui::Stroke::new(1.0, CLR_BORDER))
                    .rounding(egui::Rounding::same(12.0)))
                .show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Что вы хотите сделать?").color(CLR_TEXT));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(
                        "При сворачивании в трей программа продолжает работать\nи вы сможете принимать звонки.")
                        .small().color(CLR_TEXT_DIM));
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new(
                            egui::RichText::new("Свернуть в трей").color(egui::Color32::WHITE).strong()
                        ).fill(CLR_ACCENT).min_size(egui::vec2(140.0, 36.0))).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                            self.show_close_dialog = false;
                        }
                        ui.add_space(8.0);
                        if ui.add(egui::Button::new(
                            egui::RichText::new("Выйти").color(egui::Color32::WHITE).strong()
                        ).fill(CLR_RED).min_size(egui::vec2(80.0, 36.0))).clicked() {
                            std::process::exit(0);
                        }
                        ui.add_space(8.0);
                        if ui.add(egui::Button::new(
                            egui::RichText::new("Отмена").color(CLR_TEXT)
                        ).fill(CLR_SURFACE2).min_size(egui::vec2(80.0, 36.0))).clicked() {
                            self.show_close_dialog = false;
                        }
                    });
                    ui.add_space(4.0);
                });
        }

        // ── Трей события ──────────────────────────────────────────────────
        if let Ok(_ev) = TrayIconEvent::receiver().try_recv() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if let Ok(ev) = MenuEvent::receiver().try_recv() {
            let is_open = self.tray_open_id.as_ref().map(|id| *id == ev.id).unwrap_or(false);
            let is_quit = self.tray_quit_id.as_ref().map(|id| *id == ev.id).unwrap_or(false);
            if is_open {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else if is_quit {
                std::process::exit(0);
            }
        }

        // Глобальная тема
        let mut vis = egui::Visuals::dark();
        vis.panel_fill = CLR_BG;
        vis.window_fill = CLR_SURFACE;
        vis.window_rounding = egui::Rounding::same(10.0);
        vis.widgets.noninteractive.bg_fill = CLR_SURFACE;
        vis.widgets.inactive.bg_fill = CLR_SURFACE2;
        vis.widgets.hovered.bg_fill = egui::Color32::from_rgb(40, 48, 60);
        vis.widgets.active.bg_fill = egui::Color32::from_rgb(50, 60, 75);
        vis.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, CLR_TEXT_DIM);
        vis.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, CLR_TEXT);
        vis.extreme_bg_color = egui::Color32::from_rgb(10, 14, 20);
        ctx.set_visuals(vis);

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        ctx.set_style(style);

        // Если не авторизован (имя пустое) — экран входа
        if !self.is_connected && self.cfg.display_name.is_empty() {
            egui::CentralPanel::default().frame(egui::Frame::none().fill(CLR_BG)).show(ctx, |ui| {
                self.draw_login(ui, ctx);
            });
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }

        // Авто-подключение если имя есть, но ещё не подключены
        if !self.is_connected && self.event_rx.is_none() {
            self.connect_register();
        }

        // Если идёт активный звонок / исходящий / входящий — показываем экран звонка
        match self.call_state.clone() {
            CallState::Calling { target_code: _, target_name } => {
                egui::CentralPanel::default().frame(egui::Frame::none().fill(CLR_BG)).show(ctx, |ui| {
                    self.draw_calling(ui, ctx, &target_name);
                });
                ctx.request_repaint_after(Duration::from_millis(50));
                return;
            }
            CallState::IncomingCall { from_code: _, from_name, from_peer_id } => {
                egui::CentralPanel::default().frame(egui::Frame::none().fill(CLR_BG)).show(ctx, |ui| {
                    self.draw_incoming(ui, ctx, from_peer_id, &from_name);
                });
                ctx.request_repaint_after(Duration::from_millis(50));
                return;
            }
            CallState::InCall { peer_id, peer_name, call_id, started_at } => {
                egui::CentralPanel::default().frame(egui::Frame::none().fill(CLR_BG)).show(ctx, |ui| {
                    self.draw_in_call(ui, ctx, peer_id, &peer_name, started_at, call_id);
                });
                ctx.request_repaint_after(Duration::from_millis(50));
                return;
            }
            CallState::None => {}
        }

        // Верхняя панель навигации (Чистая вкладка без костылей)
        egui::TopBottomPanel::top("topbar")
            .frame(egui::Frame::none().fill(CLR_SURFACE).inner_margin(egui::Margin::symmetric(12.0, 8.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Логотип + Имя
                    draw_chebu_icon(ui, 26.0);
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Cheburgram").size(15.0).strong().color(CLR_ACCENT));

                    ui.add_space(16.0);

                    // Вкладки навигации
                    let tab_btn = |ui: &mut egui::Ui, current: Tab, target: Tab, text: &str| -> bool {
                        let is_sel = current == target;
                        let bg = if is_sel { CLR_SURFACE2 } else { egui::Color32::TRANSPARENT };
                        let fg = if is_sel { CLR_ACCENT } else { CLR_TEXT_DIM };
                        ui.add(egui::Button::new(
                            egui::RichText::new(text).size(13.0).color(fg).strong()
                        ).fill(bg).rounding(egui::Rounding::same(6.0))).clicked()
                    };

                    if tab_btn(ui, self.active_tab, Tab::Contacts, "👥 Друзья") {
                        self.active_tab = Tab::Contacts;
                        self.request_friends_status();
                    }
                    if tab_btn(ui, self.active_tab, Tab::History, "📋 История") {
                        self.active_tab = Tab::History;
                    }
                    if tab_btn(ui, self.active_tab, Tab::Settings, "⚙ Настройки") {
                        self.active_tab = Tab::Settings;
                    }
                });
            });

        // Нижняя строка статуса
        egui::TopBottomPanel::bottom("statusbar")
            .frame(egui::Frame::none().fill(CLR_SURFACE).inner_margin(egui::Margin::symmetric(12.0, 4.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let dot_col = if self.is_connected { CLR_GREEN } else { CLR_RED };
                    let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot_rect.center(), 4.0, dot_col);
                    ui.label(egui::RichText::new(&self.status).small().color(CLR_TEXT_DIM));
                });
            });

        // Центральная панель по выбранной вкладке
        egui::CentralPanel::default().frame(egui::Frame::none().fill(CLR_BG)).show(ctx, |ui| {
            match self.active_tab {
                Tab::Contacts => self.draw_contacts(ui, ctx),
                Tab::History => self.draw_history(ui),
                Tab::Settings => self.draw_settings(ui),
            }
        });

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

// ─── Экраны ──────────────────────────────────────────────────────────────────

impl App {
    fn draw_login(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
                        .stroke(egui::Stroke::new(1.0, CLR_BORDER))
                        .show(ui, |ui| {
                            ui.set_width(280.0);
                            ui.label(egui::RichText::new("Ваше имя").color(CLR_TEXT_DIM).small());
                            ui.add_space(4.0);
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.name_input)
                                    .hint_text("Например: Amer")
                                    .desired_width(f32::INFINITY)
                                    .font(egui::FontId::proportional(15.0))
                                    .text_color(CLR_TEXT)
                            );
                            if resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                                self.connect_register();
                            }

                            ui.add_space(12.0);
                            ui.label(egui::RichText::new("Адрес сервера").color(CLR_TEXT_DIM).small());
                            ui.add_space(4.0);
                            ui.add(
                                egui::TextEdit::singleline(&mut self.cfg.server_address)
                                    .hint_text(DEFAULT_SERVER)
                                    .desired_width(f32::INFINITY)
                                    .text_color(CLR_TEXT)
                            );

                            ui.add_space(16.0);
                            let btn = egui::Button::new(
                                egui::RichText::new("Войти").size(15.0).strong().color(egui::Color32::WHITE)
                            ).fill(CLR_ACCENT).min_size(egui::vec2(f32::INFINITY, 40.0));
                            if ui.add(btn).clicked() {
                                self.connect_register();
                            }
                        });

                    if !self.status.is_empty() {
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new(&self.status).small().color(CLR_TEXT_DIM));
                    }
                });
            }
        );
    }

    fn draw_contacts(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(8.0);

        // Карточка "Мой ID"
        egui::Frame::none()
            .fill(CLR_SURFACE)
            .rounding(egui::Rounding::same(10.0))
            .stroke(egui::Stroke::new(1.0, CLR_BORDER))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Мой ID:").color(CLR_TEXT_DIM));
                    ui.label(
                        egui::RichText::new(&self.cfg.user_code)
                            .size(17.0)
                            .strong()
                            .color(CLR_ACCENT)
                            .font(egui::FontId::monospace(17.0)),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn_text = if self.copied_code_banner { "Скопировано!" } else { "📋 Скопировать" };
                        if ui.add(egui::Button::new(
                            egui::RichText::new(btn_text).small().color(CLR_TEXT)
                        ).fill(CLR_SURFACE2)).clicked() {
                            ctx.output_mut(|o| o.copied_text = self.cfg.user_code.clone());
                            self.copied_code_banner = true;
                        }
                    });
                });
            });

        ui.add_space(8.0);

        // Форма добавления друга по ID
        egui::Frame::none()
            .fill(CLR_SURFACE)
            .rounding(egui::Rounding::same(10.0))
            .stroke(egui::Stroke::new(1.0, CLR_BORDER))
            .inner_margin(egui::Margin::symmetric(14.0, 10.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("➕ Добавить по ID:").color(CLR_TEXT_DIM));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.add_friend_input)
                            .hint_text("6 цифр")
                            .desired_width(90.0)
                            .font(egui::FontId::monospace(14.0))
                            .text_color(CLR_TEXT)
                    );
                    if resp.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let code = self.add_friend_input.clone();
                        self.add_friend_by_code(&code);
                    }

                    if ui.add(egui::Button::new(
                        egui::RichText::new("Найти").strong().color(egui::Color32::WHITE)
                    ).fill(CLR_ACCENT)).clicked() {
                        let code = self.add_friend_input.clone();
                        self.add_friend_by_code(&code);
                    }
                });
            });

        ui.add_space(10.0);
        ui.label(egui::RichText::new("Мои контакты").size(15.0).strong().color(CLR_TEXT));
        ui.add_space(4.0);

        if self.cfg.friends.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label(egui::RichText::new("Список контактов пуст").size(16.0).color(CLR_TEXT_DIM));
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Введите 6-значный ID друга выше, чтобы добавить его")
                    .small().color(CLR_TEXT_DIM));
            });
            return;
        }

        let friends = self.cfg.friends.clone();
        let statuses = self.friend_statuses.clone();
        let mut call_action: Option<(String, String)> = None;
        let mut remove_action: Option<String> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            for f in &friends {
                let st = statuses.get(&f.user_code);
                let is_online = st.map(|s| s.is_online).unwrap_or(false);

                egui::Frame::none()
                    .fill(CLR_SURFACE)
                    .rounding(egui::Rounding::same(10.0))
                    .stroke(egui::Stroke::new(1.0, CLR_BORDER))
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            // Аватар
                            let col = name_color(&f.name);
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 20.0, col);
                            let first = f.name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                                first.to_string(), egui::FontId::proportional(20.0), egui::Color32::WHITE);

                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&f.name).size(15.0).strong().color(CLR_TEXT));
                                ui.horizontal(|ui| {
                                    let dot_col = if is_online { CLR_GREEN } else { CLR_TEXT_DIM };
                                    let (dot, _) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                                    ui.painter().circle_filled(dot.center(), 3.0, dot_col);
                                    let txt = if is_online { "в сети" } else { "не в сети" };
                                    ui.label(egui::RichText::new(format!("{} • ID {}", txt, f.user_code)).small().color(dot_col));
                                });
                            });

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                // Удалить контакт
                                if ui.add(egui::Button::new(
                                    egui::RichText::new("🗑").small().color(CLR_TEXT_DIM)
                                ).fill(egui::Color32::TRANSPARENT)).clicked() {
                                    remove_action = Some(f.user_code.clone());
                                }

                                ui.add_space(4.0);

                                // Позвонить
                                let btn = egui::Button::new(
                                    egui::RichText::new("Позвонить").strong().color(egui::Color32::WHITE)
                                ).fill(if is_online { CLR_GREEN } else { CLR_SURFACE2 });

                                if ui.add_enabled(is_online, btn).clicked() {
                                    call_action = Some((f.user_code.clone(), f.name.clone()));
                                }
                            });
                        });
                    });
                ui.add_space(6.0);
            }
        });

        if let Some(code) = remove_action {
            self.cfg.friends.retain(|f| f.user_code != code);
            self.friend_statuses.remove(&code);
            save_config(&self.cfg);
            self.status = "Контакт удалён".into();
        }

        if let Some((code, name)) = call_action {
            self.call_user(code, name);
        }
    }

    fn draw_calling(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, to_name: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);

            let t = ctx.input(|i| i.time) as f32;
            let pulse = ((t * 2.5).sin() * 0.15 + 0.85) as f32;
            let col = name_color(to_name);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 80.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 40.0 * pulse, col.gamma_multiply(0.3));
            ui.painter().circle_filled(rect.center(), 40.0, col);
            let first = to_name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                first.to_string(), egui::FontId::proportional(38.0), egui::Color32::WHITE);

            ui.add_space(16.0);
            ui.label(egui::RichText::new(to_name).size(22.0).strong().color(CLR_TEXT));
            ui.add_space(6.0);
            let dots = match ((t * 2.0) as u32) % 4 { 0 => ".", 1 => "..", 2 => "...", _ => "" };
            ui.label(egui::RichText::new(format!("Вызов{}", dots)).color(CLR_TEXT_DIM));
            ui.add_space(36.0);

            if ui.add(egui::Button::new(
                egui::RichText::new("Отмена").strong().color(egui::Color32::WHITE)
            ).fill(CLR_RED).min_size(egui::vec2(140.0, 42.0))).clicked() {
                self.end_call();
            }
        });
    }

    fn draw_incoming(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, from_peer_id: u32, from_name: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(50.0);

            let t = ctx.input(|i| i.time) as f32;
            let pulse = ((t * 3.0).sin() * 0.2 + 0.8) as f32;
            let col = name_color(from_name);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(90.0, 90.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 45.0 * pulse, CLR_ACCENT.gamma_multiply(0.25));
            ui.painter().circle_filled(rect.center(), 45.0, col);
            let first = from_name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                first.to_string(), egui::FontId::proportional(44.0), egui::Color32::WHITE);

            ui.add_space(14.0);
            ui.label(egui::RichText::new("Входящий звонок").small().color(CLR_TEXT_DIM));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(from_name).size(26.0).strong().color(CLR_TEXT));
            ui.add_space(32.0);

            ui.horizontal(|ui| {
                ui.add_space(30.0);
                let fn_clone = from_name.to_string();
                if ui.add(egui::Button::new(
                    egui::RichText::new("Принять").strong().color(egui::Color32::WHITE)
                ).fill(CLR_GREEN).min_size(egui::vec2(120.0, 44.0))).clicked() {
                    self.accept_call(from_peer_id, fn_clone);
                }
                ui.add_space(16.0);
                let fn2 = from_name.to_string();
                if ui.add(egui::Button::new(
                    egui::RichText::new("Отклонить").strong().color(egui::Color32::WHITE)
                ).fill(CLR_RED).min_size(egui::vec2(120.0, 44.0))).clicked() {
                    self.reject_call(from_peer_id, &fn2);
                }
            });
        });
    }

    fn draw_in_call(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context,
        _peer_id: u32, peer_name: &str, started_at: Instant, _call_id: u64)
    {
        ui.vertical_centered(|ui| {
            ui.add_space(16.0);

            let col = name_color(peer_name);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(68.0, 68.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 34.0, col);
            let first = peer_name.chars().next().unwrap_or('?').to_uppercase().next().unwrap_or('?');
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER,
                first.to_string(), egui::FontId::proportional(34.0), egui::Color32::WHITE);

            ui.add_space(8.0);
            ui.label(egui::RichText::new(peer_name).size(20.0).strong().color(CLR_TEXT));

            let el = started_at.elapsed().as_secs();
            ui.label(egui::RichText::new(format!("{:02}:{:02}", el / 60, el % 60))
                .size(13.0).color(CLR_TEXT_DIM));

            ui.add_space(20.0);

            egui::Frame::none()
                .fill(CLR_SURFACE)
                .rounding(egui::Rounding::same(10.0))
                .inner_margin(egui::Margin::same(14.0))
                .show(ui, |ui| {
                    ui.set_width(300.0);
                    let lvl = self.mic_level.load(Ordering::Relaxed);
                    let val = lvl as f32 / 100.0;
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Микрофон").small().color(CLR_TEXT_DIM));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let active_col = if lvl > 5 { CLR_GREEN } else { CLR_TEXT_DIM };
                            ui.label(egui::RichText::new(format!("{}%", lvl)).small().color(active_col));
                        });
                    });
                    ui.add_space(4.0);

                    let (vu_rect, _) = ui.allocate_exact_size(egui::vec2(272.0, 12.0), egui::Sense::hover());
                    ui.painter().rect_filled(vu_rect, egui::Rounding::same(6.0), CLR_SURFACE2);
                    let fill_w = (vu_rect.width() * val).max(0.0);
                    if fill_w > 0.0 {
                        let fill_rect = egui::Rect::from_min_size(vu_rect.min, egui::vec2(fill_w, vu_rect.height()));
                        let bar_col = if val > 0.8 { CLR_RED } else if val > 0.5 { CLR_ACCENT } else { CLR_GREEN };
                        ui.painter().rect_filled(fill_rect, egui::Rounding::same(6.0), bar_col);
                    }

                    ui.add_space(8.0);
                    let s = self.pkts_sent.load(Ordering::Relaxed);
                    let r = self.pkts_recv.load(Ordering::Relaxed);
                    ui.label(egui::RichText::new(format!("Отпр: {}  Получ: {}", s, r))
                        .small().color(CLR_TEXT_DIM));
                });

            ui.add_space(16.0);

            egui::Frame::none()
                .fill(CLR_SURFACE)
                .rounding(egui::Rounding::same(10.0))
                .inner_margin(egui::Margin::same(14.0))
                .show(ui, |ui| {
                    ui.set_width(300.0);
                    ui.horizontal(|ui| {
                        let mic_txt = if self.mic_muted { "Мик ВЫКЛ" } else { "Мик вкл" };
                        let mic_col = if self.mic_muted { egui::Color32::from_rgb(80, 30, 30) } else { CLR_SURFACE2 };
                        if ui.add(egui::Button::new(
                            egui::RichText::new(mic_txt).color(CLR_TEXT).strong()
                        ).fill(mic_col).min_size(egui::vec2(90.0, 34.0))).clicked() {
                            self.mic_muted = !self.mic_muted;
                        }

                        let spk_txt = if self.snd_muted { "Звук ВЫКЛ" } else { "Звук вкл" };
                        let spk_col = if self.snd_muted { egui::Color32::from_rgb(80, 30, 30) } else { CLR_SURFACE2 };
                        if ui.add(egui::Button::new(
                            egui::RichText::new(spk_txt).color(CLR_TEXT).strong()
                        ).fill(spk_col).min_size(egui::vec2(90.0, 34.0))).clicked() {
                            self.snd_muted = !self.snd_muted;
                        }

                        if ui.add(egui::Button::new(
                            egui::RichText::new("Завершить").strong().color(egui::Color32::WHITE)
                        ).fill(CLR_RED).min_size(egui::vec2(90.0, 34.0))).clicked() {
                            self.end_call();
                        }
                    });
                });
        });
    }

    fn draw_history(&self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.label(egui::RichText::new("История звонков").size(15.0).strong().color(CLR_TEXT));
        ui.add_space(8.0);

        if self.cfg.call_history.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(egui::RichText::new("История пуста").color(CLR_TEXT_DIM));
            });
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for r in &self.cfg.call_history {
                egui::Frame::none()
                    .fill(CLR_SURFACE)
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .stroke(egui::Stroke::new(1.0, CLR_BORDER))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let (dir_txt, dir_col) = match r.direction {
                                CallDirection::Incoming => ("  Входящий", CLR_GREEN),
                                CallDirection::Outgoing => ("Исходящий", CLR_BLUE),
                                CallDirection::Missed   => ("Пропущен ", CLR_RED),
                            };
                            let (strip, _) = ui.allocate_exact_size(egui::vec2(3.0, 36.0), egui::Sense::hover());
                            ui.painter().rect_filled(strip, egui::Rounding::same(2.0), dir_col);
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&r.peer_name).strong().color(CLR_TEXT));
                                ui.label(egui::RichText::new(dir_txt).small().color(dir_col));
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let dur = if r.duration_secs > 0 {
                                    format!("{:02}:{:02}", r.duration_secs / 60, r.duration_secs % 60)
                                } else { "—".to_string() };
                                ui.label(egui::RichText::new(dur).small().color(CLR_TEXT_DIM));
                            });
                        });
                    });
                ui.add_space(5.0);
            }
        });
    }

    fn draw_settings(&mut self, ui: &mut egui::Ui) {
        ui.add_space(12.0);
        ui.label(egui::RichText::new("Настройки").size(15.0).strong().color(CLR_TEXT));
        ui.add_space(10.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            // Профиль
            settings_section(ui, "Профиль", |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Имя:").color(CLR_TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.name_input)
                        .desired_width(160.0).text_color(CLR_TEXT));
                    if ui.add(egui::Button::new(
                        egui::RichText::new("Сохранить").color(egui::Color32::WHITE)
                    ).fill(CLR_ACCENT)).clicked() {
                        self.cfg.display_name = self.name_input.trim().to_string();
                        save_config(&self.cfg);
                        self.status = "Имя обновлено".into();
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Ваш ID:").color(CLR_TEXT_DIM));
                    ui.label(
                        egui::RichText::new(&self.cfg.user_code)
                            .strong()
                            .color(CLR_ACCENT)
                            .font(egui::FontId::monospace(14.0)),
                    );
                });
            });

            ui.add_space(8.0);

            // Соединение
            settings_section(ui, "Сервер", |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Адрес:").color(CLR_TEXT_DIM));
                    ui.add(egui::TextEdit::singleline(&mut self.cfg.server_address)
                        .desired_width(180.0).text_color(CLR_TEXT));
                    if ui.add(egui::Button::new(
                        egui::RichText::new("OK").color(egui::Color32::WHITE)
                    ).fill(CLR_SURFACE2)).clicked() {
                        save_config(&self.cfg);
                        self.status = "Адрес сервера сохранён".into();
                    }
                });
            });

            ui.add_space(8.0);

            // Аудио
            settings_section(ui, "Аудио устрйства", |ui| {
                let prev_in = self.devs.sel_in;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Микрофон:").color(CLR_TEXT_DIM));
                    egui::ComboBox::from_id_source("mic_sel")
                        .selected_text(self.devs.inputs.get(self.devs.sel_in).cloned().unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, n) in self.devs.inputs.clone().iter().enumerate() {
                                ui.selectable_value(&mut self.devs.sel_in, i, n);
                            }
                        });
                });

                if prev_in != self.devs.sel_in && self.mic_test_active {
                    self.stop_mic_test();
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let test_col = if self.mic_test_active { CLR_RED } else { CLR_GREEN };
                    let test_txt = if self.mic_test_active { "Остановить тест" } else { "Тест микрофона" };
                    if ui.add(egui::Button::new(
                        egui::RichText::new(test_txt).color(egui::Color32::WHITE).strong()
                    ).fill(test_col).min_size(egui::vec2(160.0, 32.0))).clicked() {
                        if self.mic_test_active { self.stop_mic_test(); } else { self.start_mic_test(); }
                    }
                });

                if self.mic_test_active {
                    ui.add_space(6.0);
                    let lvl = self.mic_test_level.load(Ordering::Relaxed);
                    let val = lvl as f32 / 100.0;
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Уровень:").small().color(CLR_TEXT_DIM));
                        let (vu, _) = ui.allocate_exact_size(egui::vec2(180.0, 12.0), egui::Sense::hover());
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
                        .selected_text(self.devs.outputs.get(self.devs.sel_out).cloned().unwrap_or_default())
                        .show_ui(ui, |ui| {
                            for (i, n) in self.devs.outputs.clone().iter().enumerate() {
                                ui.selectable_value(&mut self.devs.sel_out, i, n);
                            }
                        });
                });

                ui.add_space(8.0);
                if ui.add(egui::Button::new(
                    egui::RichText::new("Сохранить аудио").color(egui::Color32::WHITE)
                ).fill(CLR_ACCENT)).clicked() {
                    self.cfg.selected_input = self.devs.sel_in;
                    self.cfg.selected_output = self.devs.sel_out;
                    save_config(&self.cfg);
                    self.status = "Настройки аудио сохранены".into();
                }
            });
        });
    }
}

fn settings_section(ui: &mut egui::Ui, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CLR_SURFACE)
        .rounding(egui::Rounding::same(10.0))
        .stroke(egui::Stroke::new(1.0, CLR_BORDER))
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
        let stroke = egui::Stroke::new(2.0, egui::Color32::WHITE);
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

fn name_color(name: &str) -> egui::Color32 {
    let h = name.bytes().fold(5381u32, |a, b| a.wrapping_mul(33).wrapping_add(b as u32));
    let hue = (h % 360) as f32;
    let s = 0.60f32; let v = 0.70f32;
    let c2 = v * s;
    let x = c2 * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = v - c2;
    let (r, g, b) = match hue as u32 {
        0..=59   => (c2, x,  0.0),
        60..=119 => (x,  c2, 0.0),
        120..=179 => (0.0, c2, x),
        180..=239 => (0.0, x,  c2),
        240..=299 => (x,  0.0, c2),
        _         => (c2, 0.0, x),
    };
    egui::Color32::from_rgb(((r+m)*255.0) as u8, ((g+m)*255.0) as u8, ((b+m)*255.0) as u8)
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in &["C:\\Windows\\Fonts\\segoeui.ttf", "C:\\Windows\\Fonts\\seguiemj.ttf"] {
        if let Ok(data) = fs::read(path) {
            let name = path.split('\\').last().unwrap_or("font").to_string();
            fonts.font_data.insert(name.clone(), egui::FontData::from_owned(data));
            for fam in [&egui::FontFamily::Proportional, &egui::FontFamily::Monospace] {
                if let Some(v) = fonts.families.get_mut(fam) { v.push(name.clone()); }
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
            if dist > r_bg { rgba[i+3] = 0; continue; }
            rgba[i] = 140; rgba[i+1] = 195; rgba[i+2] = 230; rgba[i+3] = 255;
            if (dx - (-11.0)).hypot(dy - (-1.0)) < 5.5 || (dx - 11.0).hypot(dy - (-1.0)) < 5.5 {
                rgba[i] = 166; rgba[i+1] = 110; rgba[i+2] = 71;
            }
            if dx.hypot(dy - 1.0) < 9.5 {
                rgba[i] = 166; rgba[i+1] = 110; rgba[i+2] = 71;
            }
            if dx.hypot(dy - 2.0) < 7.0 {
                rgba[i] = 245; rgba[i+1] = 214; rgba[i+2] = 184;
            }
            if (dx + 2.8).hypot(dy - 0.5) < 1.8 || (dx - 2.8).hypot(dy - 0.5) < 1.8 {
                rgba[i] = 40; rgba[i+1] = 20; rgba[i+2] = 10;
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
    info!("Cheburgram v2.2 запуск...");

    setup_autostart();

    let icon = make_app_icon();
    let vp = egui::ViewportBuilder::default()
        .with_inner_size([430.0, 600.0])
        .with_min_inner_size([380.0, 500.0])
        .with_title("Cheburgram")
        .with_resizable(true)
        .with_icon(icon);

    let options = eframe::NativeOptions { viewport: vp, ..Default::default() };

    eframe::run_native("Cheburgram", options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Box::new(App::default())
        }),
    ).map_err(|e| anyhow!("GUI: {}", e))?;
    Ok(())
}

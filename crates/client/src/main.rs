#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{anyhow, Result};
use cheburgram_protocol::{AudioPacket, CallDirection, CallRecord, ControlMessage, UserInfo};
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
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tracing::{error, info};
use uuid::Uuid;

const SAMPLE_RATE: u32 = 48000;
const FRAME_SIZE: usize = 960; // 20ms @ 48kHz
const DEFAULT_SERVER: &str = "85.192.25.57:7878";

// ─── Конфигурация (хранится в %APPDATA%\Cheburgram\config.json) ──────────────

#[derive(Serialize, Deserialize, Clone)]
struct AppConfig {
    client_id: String,
    display_name: String,
    server_address: String,
    selected_input: usize,
    selected_output: usize,
    call_history: Vec<CallRecord>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            client_id: Uuid::new_v4().to_string(),
            display_name: String::new(),
            server_address: DEFAULT_SERVER.to_string(),
            selected_input: 0,
            selected_output: 0,
            call_history: Vec::new(),
        }
    }
}

fn config_path() -> PathBuf {
    let base = dirs_or_appdata();
    fs::create_dir_all(&base).ok();
    base.join("config.json")
}

fn dirs_or_appdata() -> PathBuf {
    // %APPDATA%\Cheburgram\ или рядом с exe как запасной вариант
    if let Ok(appdata) = std::env::var("APPDATA") {
        PathBuf::from(appdata).join("Cheburgram")
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

fn load_config() -> AppConfig {
    let path = config_path();
    if path.exists() {
        if let Ok(data) = fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&data) {
                return cfg;
            }
        }
    }
    AppConfig::default()
}

fn save_config(cfg: &AppConfig) {
    if let Ok(data) = serde_json::to_string_pretty(cfg) {
        let _ = fs::write(config_path(), data);
    }
}

// ─── Состояния приложения ─────────────────────────────────────────────────────

#[derive(PartialEq, Clone)]
enum AppState {
    /// Первый запуск — ввод имени
    Login,
    /// Онлайн — список контактов
    Online,
    /// Исходящий звонок (ждём ответа)
    Calling { to_id: u32, to_name: String },
    /// Входящий звонок
    IncomingCall { from_id: u32, from_name: String },
    /// В разговоре
    InCall {
        peer_id: u32,
        peer_name: String,
        call_id: u64,
        started_at: Instant,
    },
    /// Экран истории звонков
    History,
    /// Экран настроек
    Settings,
}

// ─── Аудио устройства ────────────────────────────────────────────────────────

struct AudioDevices {
    input_names: Vec<String>,
    output_names: Vec<String>,
    selected_input: usize,
    selected_output: usize,
}

fn enumerate_audio_devices() -> AudioDevices {
    let host = cpal::default_host();
    let mut inputs = vec!["По умолчанию".to_string()];
    let mut outputs = vec!["По умолчанию".to_string()];

    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                inputs.push(name);
            }
        }
    }
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Ok(name) = d.name() {
                outputs.push(name);
            }
        }
    }
    AudioDevices {
        input_names: inputs,
        output_names: outputs,
        selected_input: 0,
        selected_output: 0,
    }
}

// ─── Главный класс приложения ─────────────────────────────────────────────────

struct CheburgramApp {
    config: AppConfig,
    state: AppState,
    status_msg: String,

    // Ввод имени
    name_input: String,

    // Контакты онлайн
    contacts: Vec<UserInfo>,
    my_peer_id: Option<u32>,

    // Сеть
    event_rx: Option<Receiver<ControlMessage>>,
    stop_signal: Arc<AtomicBool>,

    // Аудио
    audio_devices: AudioDevices,
    mic_level: Arc<AtomicU8>,
    mic_muted: bool,
    sound_muted: bool,
    packets_sent: Arc<AtomicU64>,
    packets_recv: Arc<AtomicU64>,
    udp_socket: Option<Arc<StdUdpSocket>>,
    call_id_atomic: Arc<AtomicU64>,

    // Ожидаем call_id от сервера после принятия звонка
    pending_call_peer: Option<(u32, String)>,

    // Инициирован звонок: сохраняем начало для истории
    call_start: Option<Instant>,
}

impl Default for CheburgramApp {
    fn default() -> Self {
        let cfg = load_config();
        let initial_state = if cfg.display_name.is_empty() {
            AppState::Login
        } else {
            AppState::Login // всегда через логин, но имя уже заполнено
        };
        let name_input = cfg.display_name.clone();
        let audio_devices = {
            let mut d = enumerate_audio_devices();
            d.selected_input = cfg.selected_input.min(d.input_names.len().saturating_sub(1));
            d.selected_output = cfg.selected_output.min(d.output_names.len().saturating_sub(1));
            d
        };

        Self {
            config: cfg,
            state: initial_state,
            status_msg: String::new(),
            name_input,
            contacts: Vec::new(),
            my_peer_id: None,
            event_rx: None,
            stop_signal: Arc::new(AtomicBool::new(false)),
            audio_devices,
            mic_level: Arc::new(AtomicU8::new(0)),
            mic_muted: false,
            sound_muted: false,
            packets_sent: Arc::new(AtomicU64::new(0)),
            packets_recv: Arc::new(AtomicU64::new(0)),
            udp_socket: None,
            call_id_atomic: Arc::new(AtomicU64::new(0)),
            pending_call_peer: None,
            call_start: None,
        }
    }
}

impl CheburgramApp {
    // ── Подключение к серверу ────────────────────────────────────────────────
    fn connect_and_register(&mut self) {
        let name = self.name_input.trim().to_string();
        if name.is_empty() {
            self.status_msg = "Введите имя!".to_string();
            return;
        }

        self.config.display_name = name.clone();
        save_config(&self.config);

        let addr = normalize_addr(&self.config.server_address);
        self.status_msg = format!("Подключение к {}…", addr);
        self.stop_signal.store(false, Ordering::SeqCst);

        let (tx, rx) = channel::<ControlMessage>();
        self.event_rx = Some(rx);

        let client_id = self.config.client_id.clone();
        let stop = self.stop_signal.clone();

        thread::spawn(move || {
            match StdTcpStream::connect(&addr) {
                Ok(mut stream) => {
                    let msg = ControlMessage::Register { client_id, name };
                    let json = serde_json::to_string(&msg).unwrap();
                    if stream.write_all(format!("{}\n", json).as_bytes()).is_err() {
                        let _ = tx.send(ControlMessage::Error {
                            message: "Ошибка отправки регистрации".to_string(),
                        });
                        return;
                    }

                    let read_stream = match stream.try_clone() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    let mut reader = BufReader::new(read_stream);
                    let mut line = String::new();

                    while !stop.load(Ordering::Relaxed) {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                if let Ok(msg) = serde_json::from_str::<ControlMessage>(&line) {
                                    if tx.send(msg).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(ControlMessage::Error {
                        message: format!("Не удалось подключиться: {}", e),
                    });
                }
            }
        });
    }

    // ── Отправить команду серверу (через TCP поток в фоне) ────────────────────
    // Для команд после регистрации нужен отдельный канал записи.
    // Пока используем прямой вызов TCP из нового потока (упрощённо).
    fn send_command(&self, msg: ControlMessage) {
        let addr = normalize_addr(&self.config.server_address);
        thread::spawn(move || {
            if let Ok(mut stream) = StdTcpStream::connect(&addr) {
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = stream.write_all(format!("{}\n", json).as_bytes());
                }
            }
        });
    }

    // ── Инициировать звонок ──────────────────────────────────────────────────
    fn call_user(&mut self, to_id: u32, to_name: String) {
        self.state = AppState::Calling {
            to_id,
            to_name: to_name.clone(),
        };
        self.status_msg = format!("Вызов {}…", to_name);
        self.call_start = Some(Instant::now());
        self.send_command(ControlMessage::CallRequest { to_id });
    }

    // ── Принять входящий звонок ──────────────────────────────────────────────
    fn accept_call(&mut self, from_id: u32, from_name: String) {
        self.pending_call_peer = Some((from_id, from_name.clone()));
        self.call_start = Some(Instant::now());
        self.send_command(ControlMessage::CallAccept { to_id: from_id });
        self.status_msg = format!("Соединяемся с {}…", from_name);
    }

    // ── Отклонить звонок ─────────────────────────────────────────────────────
    fn reject_call(&mut self, from_id: u32) {
        self.send_command(ControlMessage::CallReject { to_id: from_id });
        self.state = AppState::Online;
        self.status_msg = "Звонок отклонён".to_string();
    }

    // ── Завершить звонок ─────────────────────────────────────────────────────
    fn end_call(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        self.udp_socket = None;

        // Записываем в историю
        let (peer_name, direction, started_at) = match &self.state {
            AppState::InCall { peer_name, started_at, .. } => {
                (peer_name.clone(), CallDirection::Outgoing, *started_at)
            }
            AppState::Calling { to_name, .. } => {
                (to_name.clone(), CallDirection::Outgoing, self.call_start.unwrap_or_else(Instant::now))
            }
            _ => {
                self.state = AppState::Online;
                return;
            }
        };

        let duration = started_at.elapsed().as_secs();
        self.config.call_history.insert(0, CallRecord {
            peer_name,
            direction,
            timestamp: Utc::now().to_rfc3339(),
            duration_secs: duration,
        });
        if self.config.call_history.len() > 50 {
            self.config.call_history.truncate(50);
        }
        save_config(&self.config);

        self.send_command(ControlMessage::CallEnd);
        self.state = AppState::Online;
        self.status_msg = "Звонок завершён".to_string();
        self.mic_level.store(0, Ordering::Relaxed);
        self.packets_sent.store(0, Ordering::Relaxed);
        self.packets_recv.store(0, Ordering::Relaxed);
    }

    // ── Запуск аудио ─────────────────────────────────────────────────────────
    fn start_audio(&mut self, peer_id: u32, server_ip: &str, udp_port: u16, call_id: u64) {
        let socket = match StdUdpSocket::bind("0.0.0.0:0") {
            Ok(s) => Arc::new(s),
            Err(e) => {
                self.status_msg = format!("UDP ошибка: {}", e);
                return;
            }
        };

        let target: SocketAddr = format!("{}:{}", server_ip, udp_port)
            .parse()
            .unwrap_or_else(|_| "85.192.25.57:7879".parse().unwrap());

        self.udp_socket = Some(socket.clone());
        self.call_id_atomic.store(call_id, Ordering::Relaxed);
        self.stop_signal.store(false, Ordering::SeqCst);

        let my_id = self.my_peer_id.unwrap_or(0);

        // Регистрация UDP (ping каждые 2с)
        {
            let sock = socket.clone();
            let stop = self.stop_signal.clone();
            let cid = call_id;
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let pkt = AudioPacket::new(cid, my_id, 0, vec![]);
                    if let Ok(b) = pkt.to_bytes() {
                        let _ = sock.send_to(&b, target);
                    }
                    thread::sleep(Duration::from_secs(2));
                }
            });
        }

        // Захват микрофона
        let input_name = self.audio_devices.input_names
            .get(self.audio_devices.selected_input)
            .cloned()
            .unwrap_or_default();
        {
            let sock = socket.clone();
            let stop = self.stop_signal.clone();
            let level = self.mic_level.clone();
            let sent = self.packets_sent.clone();
            let muted = self.mic_muted;
            thread::spawn(move || {
                if let Err(e) = audio_input_loop(input_name, sock, target, call_id, my_id, stop, level, sent, muted) {
                    error!("Ошибка микрофона: {:?}", e);
                }
            });
        }

        // Воспроизведение
        let output_name = self.audio_devices.output_names
            .get(self.audio_devices.selected_output)
            .cloned()
            .unwrap_or_default();
        {
            let sock = socket.clone();
            let stop = self.stop_signal.clone();
            let recv = self.packets_recv.clone();
            let call_id_a = self.call_id_atomic.clone();
            thread::spawn(move || {
                if let Err(e) = audio_output_loop(output_name, sock, stop, recv, call_id_a) {
                    error!("Ошибка вывода: {:?}", e);
                }
            });
        }
    }

    // ── Обработка сетевых событий ────────────────────────────────────────────
    fn poll_network(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = &self.event_rx {
            while let Ok(msg) = rx.try_recv() {
                events.push(msg);
            }
        }

        for msg in events {
            match msg {
                ControlMessage::Registered { peer_id, udp_port: _ } => {
                    self.my_peer_id = Some(peer_id);
                    self.state = AppState::Online;
                    self.status_msg = format!(
                        "Онлайн как {} (peer #{})",
                        self.config.display_name, peer_id
                    );
                }

                ControlMessage::UserList { users } => {
                    let my_id = self.my_peer_id;
                    self.contacts = users
                        .into_iter()
                        .filter(|u| Some(u.peer_id) != my_id)
                        .collect();
                }

                ControlMessage::UserOnline { peer_id, name } => {
                    if Some(peer_id) != self.my_peer_id
                        && !self.contacts.iter().any(|c| c.peer_id == peer_id)
                    {
                        self.contacts.push(UserInfo { peer_id, name });
                    }
                }

                ControlMessage::UserOffline { peer_id, .. } => {
                    self.contacts.retain(|c| c.peer_id != peer_id);
                }

                ControlMessage::IncomingCall { from_id, from_name } => {
                    self.state = AppState::IncomingCall { from_id, from_name };
                }

                ControlMessage::CallAccepted { peer_id, peer_name } => {
                    // Получили подтверждение — запускаем аудио
                    let call_id = rand_call_id();
                    let server_ip = self.config.server_address
                        .split(':')
                        .next()
                        .unwrap_or("85.192.25.57")
                        .to_string();

                    self.start_audio(peer_id, &server_ip, 7879, call_id);
                    let started = self.call_start.unwrap_or_else(Instant::now);
                    self.state = AppState::InCall {
                        peer_id,
                        peer_name: peer_name.clone(),
                        call_id,
                        started_at: started,
                    };
                    self.status_msg = format!("В разговоре с {}", peer_name);
                    self.pending_call_peer = None;
                }

                ControlMessage::CallRejected { peer_name, .. } => {
                    self.state = AppState::Online;
                    self.status_msg = format!("{} отклонил звонок", peer_name);
                    // Добавляем в историю как пропущенный
                    self.config.call_history.insert(0, CallRecord {
                        peer_name,
                        direction: CallDirection::Outgoing,
                        timestamp: Utc::now().to_rfc3339(),
                        duration_secs: 0,
                    });
                    save_config(&self.config);
                }

                ControlMessage::CallEnded { peer_name } => {
                    self.stop_signal.store(true, Ordering::SeqCst);
                    self.udp_socket = None;
                    let duration = self.call_start.map(|s| s.elapsed().as_secs()).unwrap_or(0);
                    self.config.call_history.insert(0, CallRecord {
                        peer_name: peer_name.clone(),
                        direction: CallDirection::Incoming,
                        timestamp: Utc::now().to_rfc3339(),
                        duration_secs: duration,
                    });
                    if self.config.call_history.len() > 50 {
                        self.config.call_history.truncate(50);
                    }
                    save_config(&self.config);
                    self.state = AppState::Online;
                    self.status_msg = format!("{} завершил звонок", peer_name);
                }

                ControlMessage::Error { message } => {
                    self.status_msg = format!("⚠ {}", message);
                    if matches!(self.state, AppState::Calling { .. }) {
                        self.state = AppState::Online;
                    }
                }

                _ => {}
            }
        }
    }
}

fn rand_call_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

fn normalize_addr(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return DEFAULT_SERVER.to_string();
    }
    if s.ends_with(":22") {
        return format!("{}:7878", s.trim_end_matches(":22"));
    }
    if !s.contains(':') {
        return format!("{}:7878", s);
    }
    s.to_string()
}

// ─── Аудио петли ─────────────────────────────────────────────────────────────

fn audio_input_loop(
    device_name: String,
    socket: Arc<StdUdpSocket>,
    target: SocketAddr,
    call_id: u64,
    my_id: u32,
    stop: Arc<AtomicBool>,
    level: Arc<AtomicU8>,
    sent: Arc<AtomicU64>,
    muted: bool,
) -> Result<()> {
    let host = cpal::default_host();
    let device = if !device_name.is_empty() && device_name != "По умолчанию" {
        host.input_devices()?
            .find(|d| d.name().ok().as_deref() == Some(&device_name))
            .ok_or_else(|| anyhow!("Микрофон не найден: {}", device_name))?
    } else {
        host.default_input_device().ok_or_else(|| anyhow!("Нет микрофона"))?
    };

    let default_cfg = device.default_input_config()?;
    let sample_fmt = default_cfg.sample_format();
    let config: cpal::StreamConfig = default_cfg.into();
    let channels = config.channels as usize;

    let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
    let pcm = Arc::new(Mutex::new(Vec::<f32>::with_capacity(FRAME_SIZE * 4)));
    let pcm_c = pcm.clone();
    let err_fn = |e| error!("CPAL input: {}", e);

    let stream = match sample_fmt {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _: &_| {
                let mut buf = pcm_c.lock().unwrap();
                if channels == 1 {
                    buf.extend_from_slice(data);
                } else {
                    for chunk in data.chunks(channels) {
                        buf.push(chunk.iter().sum::<f32>() / channels as f32);
                    }
                }
            },
            err_fn, None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _: &_| {
                let mut buf = pcm_c.lock().unwrap();
                if channels == 1 {
                    buf.extend(data.iter().map(|&s| s as f32 / 32768.0));
                } else {
                    for chunk in data.chunks(channels) {
                        let avg = chunk.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / channels as f32;
                        buf.push(avg);
                    }
                }
            },
            err_fn, None,
        )?,
        _ => return Err(anyhow!("Неподдерживаемый формат аудио")),
    };
    stream.play()?;

    let mut seq = 1u64;
    let mut opus_buf = vec![0u8; 4000];

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(10));

        let samples: Vec<f32> = {
            let mut buf = pcm.lock().unwrap();
            if buf.len() < FRAME_SIZE { continue; }
            buf.drain(..FRAME_SIZE).collect()
        };

        // VU meter
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        level.store(((rms * 20.0).sqrt() * 100.0).min(100.0) as u8, Ordering::Relaxed);

        if muted { continue; }

        if let Ok(n) = encoder.encode_float(&samples, &mut opus_buf) {
            let pkt = AudioPacket::new(call_id, my_id, seq, opus_buf[..n].to_vec());
            if let Ok(bytes) = pkt.to_bytes() {
                let _ = socket.send_to(&bytes, target);
                seq += 1;
                sent.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    Ok(())
}

fn audio_output_loop(
    device_name: String,
    socket: Arc<StdUdpSocket>,
    stop: Arc<AtomicBool>,
    recv: Arc<AtomicU64>,
    call_id_a: Arc<AtomicU64>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = if !device_name.is_empty() && device_name != "По умолчанию" {
        host.output_devices()?
            .find(|d| d.name().ok().as_deref() == Some(&device_name))
            .ok_or_else(|| anyhow!("Динамики не найдены: {}", device_name))?
    } else {
        host.default_output_device().ok_or_else(|| anyhow!("Нет динамиков"))?
    };

    let config: cpal::StreamConfig = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    let ring = HeapRb::<f32>::new(SAMPLE_RATE as usize);
    let (mut producer, mut consumer) = ring.split();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &_| {
            for s in data.iter_mut() {
                *s = consumer.pop().unwrap_or(0.0);
            }
        },
        |e| error!("CPAL output: {}", e),
        None,
    )?;
    stream.play()?;

    let mut decoder = Decoder::new(SAMPLE_RATE, Channels::Mono)?;
    let mut sock_buf = vec![0u8; 65535];
    let mut pcm_out = vec![0.0f32; FRAME_SIZE * 2];

    socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    while !stop.load(Ordering::Relaxed) {
        if let Ok((n, _)) = socket.recv_from(&mut sock_buf) {
            if let Ok(pkt) = AudioPacket::from_bytes(&sock_buf[..n]) {
                let current_call_id = call_id_a.load(Ordering::Relaxed);
                if pkt.payload.is_empty() || pkt.header.room_id != current_call_id {
                    continue;
                }
                if let Ok(decoded) = decoder.decode_float(&pkt.payload, &mut pcm_out, false) {
                    for &s in &pcm_out[..decoded] {
                        let _ = producer.push(s);
                    }
                    recv.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    Ok(())
}

// ─── GUI ─────────────────────────────────────────────────────────────────────

impl eframe::App for CheburgramApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_network();

        // Тема
        let mut visual = egui::Visuals::dark();
        visual.window_rounding = egui::Rounding::same(12.0);
        visual.panel_fill = egui::Color32::from_rgb(18, 18, 30);
        ctx.set_visuals(visual);

        // Верхняя панель
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("🎙 CHEBURGRAM")
                        .size(18.0)
                        .strong()
                        .color(egui::Color32::from_rgb(255, 140, 0)),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !matches!(self.state, AppState::Login) {
                        let settings_active = matches!(self.state, AppState::Settings);
                        let history_active = matches!(self.state, AppState::History);
                        if ui.selectable_label(settings_active, "⚙").clicked() {
                            self.state = if settings_active {
                                AppState::Online
                            } else {
                                AppState::Settings
                            };
                        }
                        if ui.selectable_label(history_active, "📋").clicked() {
                            self.state = if history_active {
                                AppState::Online
                            } else {
                                AppState::History
                            };
                        }
                    }
                });
            });
        });

        // Нижняя строка статуса
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(&self.status_msg)
                        .small()
                        .color(egui::Color32::GRAY),
                );
            });
        });

        // Центральная панель
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.state.clone() {
                AppState::Login => self.ui_login(ui),
                AppState::Online => self.ui_contacts(ui),
                AppState::Calling { to_id, to_name } => self.ui_calling(ui, to_id, &to_name),
                AppState::IncomingCall { from_id, from_name } => {
                    self.ui_incoming(ui, from_id, &from_name)
                }
                AppState::InCall { peer_name, started_at, call_id, peer_id } => {
                    self.ui_in_call(ui, peer_id, &peer_name, started_at, call_id)
                }
                AppState::History => self.ui_history(ui),
                AppState::Settings => self.ui_settings(ui),
            }
        });

        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

impl CheburgramApp {
    fn ui_login(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);
            ui.label(
                egui::RichText::new("Добро пожаловать!")
                    .size(22.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Введите ваше имя для первого входа")
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(30.0);

            ui.group(|ui| {
                ui.set_max_width(280.0);
                ui.add_space(10.0);
                ui.label("Ваше имя:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.name_input)
                        .hint_text("Например: Amer")
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                {
                    self.connect_and_register();
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Сервер:")
                        .small()
                        .color(egui::Color32::GRAY),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.config.server_address)
                        .hint_text(DEFAULT_SERVER)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(12.0);
                if ui
                    .add_sized(
                        [f32::INFINITY, 38.0],
                        egui::Button::new(
                            egui::RichText::new("Войти →").strong().size(16.0),
                        )
                        .fill(egui::Color32::from_rgb(255, 140, 0)),
                    )
                    .clicked()
                {
                    self.connect_and_register();
                }
                ui.add_space(10.0);
            });
        });
    }

    fn ui_contacts(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);

        if self.contacts.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(80.0);
                ui.label(
                    egui::RichText::new("Никого нет онлайн")
                        .size(16.0)
                        .color(egui::Color32::GRAY),
                );
                ui.label(
                    egui::RichText::new("Когда другие участники подключатся, они появятся здесь")
                        .small()
                        .color(egui::Color32::from_rgb(100, 100, 100)),
                );
            });
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let contacts = self.contacts.clone();
            for contact in &contacts {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        // Аватар-кружок с первой буквой
                        let first_char = contact.name.chars().next().unwrap_or('?');
                        let avatar_color = name_to_color(&contact.name);
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(36.0, 36.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().circle_filled(rect.center(), 18.0, avatar_color);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            first_char.to_uppercase().to_string(),
                            egui::FontId::proportional(18.0),
                            egui::Color32::WHITE,
                        );

                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(&contact.name)
                                    .strong()
                                    .size(15.0),
                            );
                            ui.label(
                                egui::RichText::new("🟢 Онлайн")
                                    .small()
                                    .color(egui::Color32::GREEN),
                            );
                        });

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let call_btn = ui.add_sized(
                                [80.0, 32.0],
                                egui::Button::new(
                                    egui::RichText::new("📞 Звонок").strong(),
                                )
                                .fill(egui::Color32::from_rgb(0, 160, 80)),
                            );
                            if call_btn.clicked() {
                                let id = contact.peer_id;
                                let name = contact.name.clone();
                                self.call_user(id, name);
                            }
                        });
                    });
                });
                ui.add_space(4.0);
            }
        });
    }

    fn ui_calling(&mut self, ui: &mut egui::Ui, to_id: u32, to_name: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(60.0);

            // Анимированный пульс — просто меняем прозрачность
            let t = ui.input(|i| i.time);
            let alpha = ((t * 2.0).sin() * 0.5 + 0.5) as f32;
            let col = egui::Color32::from_rgba_unmultiplied(
                0, 200, 100, (alpha * 255.0) as u8,
            );
            ui.label(egui::RichText::new("📞").size(64.0).color(col));

            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(format!("Вызов {to_name}…"))
                    .size(20.0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Ждём ответа…")
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(30.0);

            if ui
                .add_sized(
                    [140.0, 40.0],
                    egui::Button::new(egui::RichText::new("❌ Отмена").strong())
                        .fill(egui::Color32::from_rgb(180, 30, 30)),
                )
                .clicked()
            {
                self.send_command(ControlMessage::CallReject { to_id });
                self.state = AppState::Online;
                self.status_msg = "Звонок отменён".to_string();
            }
        });
    }

    fn ui_incoming(&mut self, ui: &mut egui::Ui, from_id: u32, from_name: &str) {
        ui.vertical_centered(|ui| {
            ui.add_space(50.0);

            // Пульсирующий звонок
            let t = ui.input(|i| i.time);
            let alpha = ((t * 3.0).sin() * 0.5 + 0.5) as f32;
            let col = egui::Color32::from_rgba_unmultiplied(
                255, 140, 0, (alpha * 255.0) as u8,
            );
            ui.label(egui::RichText::new("📲").size(72.0).color(col));

            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("ВХОДЯЩИЙ ЗВОНОК")
                    .size(14.0)
                    .color(egui::Color32::GRAY),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(from_name)
                    .size(28.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            );
            ui.add_space(30.0);

            ui.horizontal(|ui| {
                ui.add_space(40.0);
                if ui
                    .add_sized(
                        [120.0, 44.0],
                        egui::Button::new(
                            egui::RichText::new("✅ Принять").strong().size(16.0),
                        )
                        .fill(egui::Color32::from_rgb(0, 160, 80)),
                    )
                    .clicked()
                {
                    let fid = from_id;
                    let fname = from_name.to_string();
                    self.accept_call(fid, fname);
                }
                ui.add_space(16.0);
                if ui
                    .add_sized(
                        [120.0, 44.0],
                        egui::Button::new(
                            egui::RichText::new("❌ Отклонить").strong().size(16.0),
                        )
                        .fill(egui::Color32::from_rgb(180, 30, 30)),
                    )
                    .clicked()
                {
                    self.reject_call(from_id);
                    // Записываем пропущенный
                    self.config.call_history.insert(0, CallRecord {
                        peer_name: from_name.to_string(),
                        direction: CallDirection::Missed,
                        timestamp: Utc::now().to_rfc3339(),
                        duration_secs: 0,
                    });
                    save_config(&self.config);
                }
            });
        });
    }

    fn ui_in_call(
        &mut self,
        ui: &mut egui::Ui,
        peer_id: u32,
        peer_name: &str,
        started_at: Instant,
        call_id: u64,
    ) {
        ui.vertical_centered(|ui| {
            ui.add_space(10.0);

            // Аватар собеседника
            let first = peer_name.chars().next().unwrap_or('?');
            let col = name_to_color(peer_name);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 32.0, col);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                first.to_uppercase().to_string(),
                egui::FontId::proportional(32.0),
                egui::Color32::WHITE,
            );

            ui.label(
                egui::RichText::new(peer_name)
                    .size(22.0)
                    .strong()
                    .color(egui::Color32::WHITE),
            );

            let elapsed = started_at.elapsed().as_secs();
            ui.label(
                egui::RichText::new(format!("⏱ {:02}:{:02}", elapsed / 60, elapsed % 60))
                    .size(14.0)
                    .color(egui::Color32::GRAY),
            );

            ui.add_space(14.0);

            // VU meter
            let mic_pct = self.mic_level.load(Ordering::Relaxed);
            let mic_val = mic_pct as f32 / 100.0;
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("🎙")
                        .color(if mic_pct > 5 { egui::Color32::GREEN } else { egui::Color32::GRAY }),
                );
                ui.add(
                    egui::ProgressBar::new(mic_val)
                        .fill(egui::Color32::from_rgb(0, 180, 100))
                        .desired_width(200.0),
                );
                ui.label(egui::RichText::new(format!("{}%", mic_pct)).small());
            });

            ui.add_space(4.0);
            let sent = self.packets_sent.load(Ordering::Relaxed);
            let rcvd = self.packets_recv.load(Ordering::Relaxed);
            ui.label(
                egui::RichText::new(format!("↑ {} пак  ↓ {} пак", sent, rcvd))
                    .small()
                    .color(egui::Color32::GRAY),
            );

            ui.add_space(16.0);

            // Кнопки управления
            ui.horizontal(|ui| {
                ui.add_space(20.0);

                let mic_label = if self.mic_muted { "🔇 Мик" } else { "🎙 Мик" };
                let mic_color = if self.mic_muted {
                    egui::Color32::from_rgb(100, 100, 100)
                } else {
                    egui::Color32::from_rgb(50, 50, 80)
                };
                if ui
                    .add_sized([90.0, 36.0], egui::Button::new(mic_label).fill(mic_color))
                    .clicked()
                {
                    self.mic_muted = !self.mic_muted;
                }

                ui.add_space(8.0);

                let spk_label = if self.sound_muted { "🔇 Звук" } else { "🔊 Звук" };
                let spk_color = if self.sound_muted {
                    egui::Color32::from_rgb(100, 100, 100)
                } else {
                    egui::Color32::from_rgb(50, 50, 80)
                };
                if ui
                    .add_sized([90.0, 36.0], egui::Button::new(spk_label).fill(spk_color))
                    .clicked()
                {
                    self.sound_muted = !self.sound_muted;
                }

                ui.add_space(8.0);

                if ui
                    .add_sized(
                        [110.0, 36.0],
                        egui::Button::new(
                            egui::RichText::new("🔴 Завершить").strong().color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(200, 30, 30)),
                    )
                    .clicked()
                {
                    self.end_call();
                }
            });
        });
    }

    fn ui_history(&self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("📋 История звонков").strong().size(16.0));
        ui.separator();
        ui.add_space(4.0);

        if self.config.call_history.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(60.0);
                ui.label(egui::RichText::new("История пуста").color(egui::Color32::GRAY));
            });
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for record in &self.config.call_history {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        let (icon, color) = match record.direction {
                            CallDirection::Incoming => ("↙ Входящий", egui::Color32::GREEN),
                            CallDirection::Outgoing => ("↗ Исходящий", egui::Color32::from_rgb(100, 160, 255)),
                            CallDirection::Missed => ("✕ Пропущен", egui::Color32::RED),
                        };
                        ui.label(egui::RichText::new(icon).small().color(color));
                        ui.separator();
                        ui.label(egui::RichText::new(&record.peer_name).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if record.duration_secs > 0 {
                                let m = record.duration_secs / 60;
                                let s = record.duration_secs % 60;
                                ui.label(
                                    egui::RichText::new(format!("{:02}:{:02}", m, s))
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("нет ответа")
                                        .small()
                                        .color(egui::Color32::GRAY),
                                );
                            }
                        });
                    });
                });
            }
        });
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("⚙ Настройки").strong().size(16.0));
        ui.separator();
        ui.add_space(8.0);

        ui.group(|ui| {
            ui.label(egui::RichText::new("Профиль").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Имя:");
                ui.text_edit_singleline(&mut self.name_input);
                if ui.button("Обновить").clicked() {
                    self.config.display_name = self.name_input.trim().to_string();
                    save_config(&self.config);
                    self.status_msg = "Имя обновлено".to_string();
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("ID устройства:");
                ui.label(
                    egui::RichText::new(&self.config.client_id[..16])
                        .small()
                        .color(egui::Color32::GRAY),
                );
            });
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label(egui::RichText::new("Соединение").strong());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Сервер:");
                ui.text_edit_singleline(&mut self.config.server_address);
                if ui.button("Сохранить").clicked() {
                    save_config(&self.config);
                }
            });
        });

        ui.add_space(8.0);
        ui.group(|ui| {
            ui.label(egui::RichText::new("Аудио").strong());
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("Микрофон:");
                egui::ComboBox::from_id_source("mic")
                    .selected_text(
                        self.audio_devices.input_names
                            .get(self.audio_devices.selected_input)
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .show_ui(ui, |ui| {
                        for (i, name) in self.audio_devices.input_names.iter().enumerate() {
                            ui.selectable_value(&mut self.audio_devices.selected_input, i, name);
                        }
                    });
            });

            ui.horizontal(|ui| {
                ui.label("Динамики:");
                egui::ComboBox::from_id_source("spk")
                    .selected_text(
                        self.audio_devices.output_names
                            .get(self.audio_devices.selected_output)
                            .cloned()
                            .unwrap_or_default(),
                    )
                    .show_ui(ui, |ui| {
                        for (i, name) in self.audio_devices.output_names.iter().enumerate() {
                            ui.selectable_value(&mut self.audio_devices.selected_output, i, name);
                        }
                    });
            });

            if ui.button("Сохранить аудио").clicked() {
                self.config.selected_input = self.audio_devices.selected_input;
                self.config.selected_output = self.audio_devices.selected_output;
                save_config(&self.config);
                self.status_msg = "Настройки аудио сохранены".to_string();
            }
        });
    }
}

/// Генерация цвета аватара по имени
fn name_to_color(name: &str) -> egui::Color32 {
    let hash = name.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    let hue = (hash % 360) as f32;
    // HSV → RGB (упрощённо, S=0.7, V=0.8)
    let s = 0.65f32;
    let v = 0.75f32;
    let c = v * s;
    let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match hue as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    egui::Color32::from_rgb(
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for path in &[
        "C:\\Windows\\Fonts\\seguiemj.ttf",
        "C:\\Windows\\Fonts\\segoeui.ttf",
    ] {
        if let Ok(data) = fs::read(path) {
            let name = path.split('\\').last().unwrap_or("font").to_string();
            fonts.font_data.insert(name.clone(), egui::FontData::from_owned(data));
            if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                v.push(name.clone());
            }
            if let Some(v) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                v.push(name);
            }
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Запуск Cheburgram v2...");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 560.0])
            .with_min_inner_size([380.0, 480.0])
            .with_title("Cheburgram")
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "Cheburgram",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Box::new(CheburgramApp::default())
        }),
    )
    .map_err(|e| anyhow!("GUI ошибка: {}", e))?;

    Ok(())
}

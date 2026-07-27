use anyhow::{anyhow, Result};
use cheburgram_protocol::{AudioPacket, ControlMessage};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eframe::egui;
use opus::{Application, Channels, Decoder, Encoder};
use ringbuf::HeapRb;
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream as StdTcpStream, UdpSocket as StdUdpSocket},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use tracing::{error, info};

const SAMPLE_RATE: u32 = 48000;
const FRAME_SIZE: usize = 960; // 20ms at 48kHz

#[derive(PartialEq, Clone, Copy)]
enum AppState {
    Disconnected,
    Connecting,
    InRoom,
}

struct AudioDevices {
    input_devices: Vec<String>,
    output_devices: Vec<String>,
    selected_input: usize,
    selected_output: usize,
}

struct CheburgramApp {
    server_address: String,
    room_code_input: String,
    current_room: String,
    peer_id: u32,
    app_state: AppState,
    status_message: String,
    peer_connected: bool,

    // Аудио параметры
    mic_muted: bool,
    sound_muted: bool,
    mic_level: Arc<AtomicU8>, // 0..100 VU meter

    // Сетевые потоки и управление
    control_writer: Option<Arc<Mutex<StdTcpStream>>>,
    udp_socket: Option<Arc<StdUdpSocket>>,
    stop_signal: Arc<AtomicBool>,

    // Статистика
    call_start_time: Option<Instant>,
    packets_sent: Arc<AtomicU64>,
    packets_recv: Arc<AtomicU64>,

    // Устройства
    devices: AudioDevices,
}

impl Default for CheburgramApp {
    fn default() -> Self {
        let (input_devices, output_devices) = get_audio_devices();
        Self {
            server_address: "127.0.0.1:7878".to_string(),
            room_code_input: String::new(),
            current_room: String::new(),
            peer_id: 0,
            app_state: AppState::Disconnected,
            status_message: "Готов к подключению".to_string(),
            peer_connected: false,

            mic_muted: false,
            sound_muted: false,
            mic_level: Arc::new(AtomicU8::new(0)),

            control_writer: None,
            udp_socket: None,
            stop_signal: Arc::new(AtomicBool::new(false)),

            call_start_time: None,
            packets_sent: Arc::new(AtomicU64::new(0)),
            packets_recv: Arc::new(AtomicU64::new(0)),

            devices: AudioDevices {
                input_devices,
                output_devices,
                selected_input: 0,
                selected_output: 0,
            },
        }
    }
}

fn get_audio_devices() -> (Vec<String>, Vec<String>) {
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

    (inputs, outputs)
}

impl CheburgramApp {
    fn create_room(&mut self) {
        self.connect_and_send(ControlMessage::CreateRoom);
    }

    fn join_room(&mut self) {
        if self.room_code_input.trim().is_empty() {
            self.status_message = "Введите код комнаты!".to_string();
            return;
        }
        let code = self.room_code_input.trim().to_uppercase();
        self.connect_and_send(ControlMessage::JoinRoom { room_code: code });
    }

    fn connect_and_send(&mut self, msg: ControlMessage) {
        self.app_state = AppState::Connecting;
        self.status_message = "Подключение к серверу...".to_string();
        self.stop_signal.store(false, Ordering::SeqCst);

        let server_addr_str = self.server_address.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<(StdTcpStream, ControlMessage), String>>();

        // Асинхронное подключение в отдельном потоке
        thread::spawn(move || {
            match StdTcpStream::connect(&server_addr_str) {
                Ok(mut stream) => {
                    let json = serde_json::to_string(&msg).unwrap();
                    if let Err(e) = stream.write_all(format!("{}\n", json).as_bytes()) {
                        let _ = tx.send(Err(format!("Ошибка отправки данных: {}", e)));
                        return;
                    }
                    let _ = tx.send(Ok((stream, msg)));
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("Не удалось подключиться: {}", e)));
                }
            }
        });

        // Слушаем результат в главном цикле
        // На время ожидания выставляем статус
    }

    fn leave_room(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        self.app_state = AppState::Disconnected;
        self.status_message = "Звонок завершен".to_string();
        self.current_room.clear();
        self.peer_connected = false;
        self.call_start_time = None;
        self.control_writer = None;
        self.udp_socket = None;
    }

    fn start_audio_and_media(
        &mut self,
        room_code: String,
        peer_id: u32,
        server_ip: String,
        udp_port: u16,
    ) {
        self.current_room = room_code.clone();
        self.peer_id = peer_id;
        self.app_state = AppState::InRoom;
        self.call_start_time = Some(Instant::now());

        // Создаем UDP сокет для отправителя и получателя
        let bind_addr = "0.0.0.0:0";
        let udp_socket = match StdUdpSocket::bind(bind_addr) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                self.status_message = format!("Ошибка UDP: {}", e);
                return;
            }
        };

        let target_udp_addr: SocketAddr = match format!("{}:{}", server_ip, udp_port).parse() {
            Ok(addr) => addr,
            Err(e) => {
                self.status_message = format!("Ошибка адреса VPS: {}", e);
                return;
            }
        };

        self.udp_socket = Some(udp_socket.clone());

        let stop_signal = self.stop_signal.clone();
        let mic_level = self.mic_level.clone();
        let packets_sent = self.packets_sent.clone();
        let packets_recv = self.packets_recv.clone();

        // 1. Поток Захвата Микрофона -> Opus -> UDP
        let socket_send = udp_socket.clone();
        let room_code_send = room_code.clone();
        thread::spawn(move || {
            if let Err(e) = run_audio_input_loop(
                socket_send,
                target_udp_addr,
                room_code_send,
                peer_id,
                stop_signal.clone(),
                mic_level,
                packets_sent,
            ) {
                error!("Ошибка аудио ввода: {:?}", e);
            }
        });

        // 2. Поток Приема UDP -> Opus -> Динамики
        let socket_recv = udp_socket.clone();
        let stop_signal_recv = self.stop_signal.clone();
        thread::spawn(move || {
            if let Err(e) = run_audio_output_loop(socket_recv, stop_signal_recv, packets_recv) {
                error!("Ошибка аудио вывода: {:?}", e);
            }
        });
    }
}

fn run_audio_input_loop(
    socket: Arc<StdUdpSocket>,
    target_addr: SocketAddr,
    room_code: String,
    peer_id: u32,
    stop_signal: Arc<AtomicBool>,
    mic_level: Arc<AtomicU8>,
    packets_sent: Arc<AtomicU64>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("Не найдено микрофона"))?;

    let config: cpal::StreamConfig = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Fixed(FRAME_SIZE as u32),
    };

    let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
    let pcm_buf = Arc::new(Mutex::new(Vec::<f32>::with_capacity(FRAME_SIZE * 2)));

    let pcm_buf_capture = pcm_buf.clone();
    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _: &_| {
            let mut buf = pcm_buf_capture.lock().unwrap();
            buf.extend_from_slice(data);
        },
        |err| error!("Ошибка CPAL микрофона: {}", err),
        None,
    )?;

    stream.play()?;

    let mut sequence = 0u64;
    let mut opus_output = vec![0u8; 4000];

    while !stop_signal.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(10));

        let mut samples_to_encode = Vec::new();
        {
            let mut buf = pcm_buf.lock().unwrap();
            if buf.len() >= FRAME_SIZE {
                samples_to_encode = buf.drain(..FRAME_SIZE).collect();
            }
        }

        if !samples_to_encode.is_empty() {
            // Расчет VU meter
            let peak = samples_to_encode
                .iter()
                .map(|s| s.abs())
                .fold(0.0f32, |a, b| a.max(b));
            mic_level.store((peak * 100.0).min(100.0) as u8, Ordering::Relaxed);

            // Кодирование Opus
            if let Ok(encoded_len) = encoder.encode_float(&samples_to_encode, &mut opus_output) {
                let packet_payload = opus_output[..encoded_len].to_vec();
                let now_ms = Instant::now().elapsed().as_millis() as u64;

                let packet = AudioPacket::new(
                    room_code.clone(),
                    peer_id,
                    sequence,
                    now_ms,
                    packet_payload,
                );

                if let Ok(bytes) = packet.to_bytes() {
                    let _ = socket.send_to(&bytes, target_addr);
                    sequence += 1;
                    packets_sent.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    Ok(())
}

fn run_audio_output_loop(
    socket: Arc<StdUdpSocket>,
    stop_signal: Arc<AtomicBool>,
    packets_recv: Arc<AtomicU64>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow!("Не найдено устройство воспроизведения"))?;

    let config: cpal::StreamConfig = cpal::StreamConfig {
        channels: 1,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Fixed(FRAME_SIZE as u32),
    };

    let ring = HeapRb::<f32>::new(SAMPLE_RATE as usize);
    let (mut producer, mut consumer) = ring.split();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &_| {
            for sample in data.iter_mut() {
                *sample = consumer.pop().unwrap_or(0.0);
            }
        },
        |err| error!("Ошибка CPAL вывода: {}", err),
        None,
    )?;

    stream.play()?;

    let mut decoder = Decoder::new(SAMPLE_RATE, Channels::Mono)?;
    let mut socket_buf = vec![0u8; 65535];
    let mut pcm_out = vec![0.0f32; FRAME_SIZE];

    socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    while !stop_signal.load(Ordering::Relaxed) {
        if let Ok((len, _)) = socket.recv_from(&mut socket_buf) {
            if let Ok(packet) = AudioPacket::from_bytes(&socket_buf[..len]) {
                if let Ok(decoded_samples) =
                    decoder.decode_float(&packet.payload, &mut pcm_out, false)
                {
                    for s in &pcm_out[..decoded_samples] {
                        let _ = producer.push(*s);
                    }
                    packets_recv.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    Ok(())
}

impl eframe::App for CheburgramApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Отрисовка стильной темной темы Cheburgram
        let mut visual = egui::Visuals::dark();
        visual.window_rounding = egui::Rounding::same(12.0);
        ctx.set_visuals(visual);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(10.0);
                ui.heading(
                    egui::RichText::new("🎙 CHEBURGRAM VOICE")
                        .size(24.0)
                        .bold()
                        .color(egui::Color32::from_rgb(255, 140, 0)),
                );
                ui.label(
                    egui::RichText::new("Безопасный голосовой созвон 1-на-1")
                        .small()
                        .italic(),
                );
                ui.add_space(15.0);
            });

            ui.separator();
            ui.add_space(10.0);

            match self.app_state {
                AppState::Disconnected => {
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("⚙️ Настройки подключения").bold());
                        ui.add_space(5.0);

                        ui.horizontal(|ui| {
                            ui.label("VPS Сервер:");
                            ui.text_edit_singleline(&mut self.server_address);
                        });
                        ui.add_space(10.0);

                        ui.columns(2, |cols| {
                            cols[0].vertical_centered(|ui| {
                                if ui
                                    .add_sized(
                                        [140.0, 40.0],
                                        egui::Button::new(
                                            egui::RichText::new("➕ Создать комнату").bold(),
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.create_room();
                                }
                            });

                            cols[1].vertical_centered(|ui| {
                                ui.text_edit_singleline(&mut self.room_code_input);
                                ui.add_space(4.0);
                                if ui
                                    .add_sized(
                                        [140.0, 30.0],
                                        egui::Button::new("🔗 Войти по коду"),
                                    )
                                    .clicked()
                                {
                                    self.join_room();
                                }
                            });
                        });
                    });

                    ui.add_space(15.0);
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("🎧 Аудио устройства").bold());
                        ui.add_space(5.0);

                        ui.horizontal(|ui| {
                            ui.label("Микрофон:  ");
                            egui::ComboBox::from_id_source("input_dev")
                                .selected_text(&self.devices.input_devices[self.devices.selected_input])
                                .show_ui(ui, |ui| {
                                    for (i, dev) in self.devices.input_devices.iter().enumerate() {
                                        ui.selectable_value(&mut self.devices.selected_input, i, dev);
                                    }
                                });
                        });

                        ui.horizontal(|ui| {
                            ui.label("Динамики:   ");
                            egui::ComboBox::from_id_source("output_dev")
                                .selected_text(&self.devices.output_devices[self.devices.selected_output])
                                .show_ui(ui, |ui| {
                                    for (i, dev) in self.devices.output_devices.iter().enumerate() {
                                        ui.selectable_value(&mut self.devices.selected_output, i, dev);
                                    }
                                });
                        });
                    });
                }
                AppState::Connecting => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.spinner();
                        ui.add_space(10.0);
                        ui.label(&self.status_message);
                    });
                }
                AppState::InRoom => {
                    ui.group(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("Код вашей комнаты:").small());
                            ui.horizontal(|ui| {
                                ui.add_space(80.0);
                                ui.heading(
                                    egui::RichText::new(&self.current_room)
                                        .size(32.0)
                                        .bold()
                                        .color(egui::Color32::LIGHT_BLUE),
                                );
                                if ui.button("📋").on_hover_text("Копировать").clicked() {
                                    ui.output_mut(|o| o.copied_text = self.current_room.clone());
                                }
                            });

                            ui.add_space(10.0);
                            if self.peer_connected {
                                ui.label(
                                    egui::RichText::new("🟢 Собеседник подключен")
                                        .color(egui::Color32::GREEN)
                                        .bold(),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("🟡 Ожидание второго участника...")
                                        .color(egui::Color32::GOLD),
                                );
                            }

                            if let Some(start) = self.call_start_time {
                                let elapsed = start.elapsed().as_secs();
                                let mins = elapsed / 60;
                                let secs = elapsed % 60;
                                ui.label(format!("Длительность: {:02}:{:02}", mins, secs));
                            }
                        });
                    });

                    ui.add_space(15.0);

                    // VU Meter уровня микрофона
                    let mic_val = self.mic_level.load(Ordering::Relaxed) as f32 / 100.0;
                    ui.label("Громкость микрофона:");
                    ui.add(egui::ProgressBar::new(mic_val).animate(true));

                    ui.add_space(20.0);

                    ui.horizontal(|ui| {
                        ui.add_space(30.0);

                        let mic_btn_text = if self.mic_muted { "🔇 Мик выкл" } else { "🎙 Мик вкл" };
                        if ui.add_sized([110.0, 35.0], egui::Button::new(mic_btn_text)).clicked() {
                            self.mic_muted = !self.mic_muted;
                        }

                        let sound_btn_text = if self.sound_muted { "🔇 Звук выкл" } else { "🔊 Звук вкл" };
                        if ui.add_sized([110.0, 35.0], egui::Button::new(sound_btn_text)).clicked() {
                            self.sound_muted = !self.sound_muted;
                        }

                        if ui
                            .add_sized(
                                [110.0, 35.0],
                                egui::Button::new(
                                    egui::RichText::new("🔴 Завершить")
                                        .color(egui::Color32::WHITE)
                                        .bold(),
                                )
                                .fill(egui::Color32::RED),
                            )
                            .clicked()
                        {
                            self.leave_room();
                        }
                    });

                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Отправлено пакетов: {} | Получено: {}",
                            self.packets_sent.load(Ordering::Relaxed),
                            self.packets_recv.load(Ordering::Relaxed)
                        ))
                        .small()
                        .weak(),
                    );
                }
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(5.0);
                ui.label(
                    egui::RichText::new(&self.status_message)
                        .small()
                        .color(egui::Color32::GRAY),
                );
            });
        });

        // Регулярное обновление UI для анимации и VU-метра
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("Запуск клиенской части Cheburgram...");

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 520.0])
            .with_min_inner_size([400.0, 480.0])
            .with_resizable(true)
            .with_title("Cheburgram Voice - 1-on-1 Call"),
        ..Default::default()
    };

    eframe::run_native(
        "Cheburgram Voice",
        native_options,
        Box::new(|_cc| Box::new(CheburgramApp::default())),
    )
    .map_err(|e| anyhow!("Ошибка запуска GUI: {}", e))?;

    Ok(())
}

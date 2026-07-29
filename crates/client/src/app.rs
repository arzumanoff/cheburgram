//! Состояние приложения и бизнес-логика.
//!
//! Архитектурная граница: UI (ui.rs) не трогает сокеты и потоки напрямую —
//! только методы App. Сеть живёт в net.rs (каналы), аудио — в cheburgram-audio
//! (атомики). Stop-флаги сети, аудио и теста микрофона раздельные.

use anyhow::Result;
use cheburgram_audio::{start_call_audio, AudioHandle, CallAudioConfig};
use cheburgram_protocol::{
    CallDirection, CallRecord, ControlMessage, FriendRequestInfo, FriendStatus, TextMessage,
};
use chrono::Utc;
use cpal::traits::{DeviceTrait, StreamTrait};
use std::collections::HashMap;
use std::net::UdpSocket as StdUdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::error;
use uuid::Uuid;

use crate::config::{
    load_config, normalize_server, save_config, server_host, AppConfig, SavedFriend,
};
use crate::net::{start as net_start, NetCredentials, NetEvent, NetHandle};

// ─── Вкладки / экраны ─────────────────────────────────────────────────────────

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Tab {
    Contacts,
    History,
    Settings,
}

#[derive(PartialEq, Clone, Debug)]
pub enum CallState {
    None,
    Calling { target_code: String, target_name: String },
    IncomingCall { from_code: String, from_name: String, from_peer_id: u32 },
    InCall {
        peer_id: u32,
        peer_name: String,
        peer_code: String,
        call_id: u64,
        started_at: Instant,
    },
}

// ─── Аудио устройства (для UI) ────────────────────────────────────────────────

pub struct AudioDevs {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub sel_in: usize,
    pub sel_out: usize,
}

impl AudioDevs {
    pub fn selected_input_name(&self) -> Option<String> {
        self.inputs.get(self.sel_in).cloned()
    }
    pub fn selected_output_name(&self) -> Option<String> {
        self.outputs.get(self.sel_out).cloned()
    }
}

fn list_audio_devs() -> AudioDevs {
    AudioDevs {
        inputs: cheburgram_audio::devices::list_inputs(),
        outputs: cheburgram_audio::devices::list_outputs(),
        sel_in: 0,
        sel_out: 0,
    }
}

// ─── Приложение ───────────────────────────────────────────────────────────────

pub struct App {
    pub cfg: AppConfig,
    pub active_tab: Tab,
    pub call_state: CallState,
    pub status: String,
    /// Получен Registered от сервера (полноценный онлайн)
    pub is_connected: bool,
    /// TCP живо (может ещё идти регистрация)
    pub link_up: bool,

    pub name_input: String,
    pub add_friend_input: String,
    pub copied_code_banner: bool,

    pub friend_statuses: HashMap<String, FriendStatus>,
    pub pending_friend_requests: Vec<FriendRequestInfo>,
    pub my_peer_id: Option<u32>,
    pub udp_port: u16,

    net: Option<NetHandle>,
    net_server: String,
    pub audio: Option<AudioHandle>,
    pub ringtone: Option<cheburgram_audio::RingtoneHandle>,

    pub devs: AudioDevs,
    pub call_start: Option<Instant>,

    // Тест микрофона
    pub mic_test_level: Arc<AtomicU8>,
    pub mic_test_stop: Arc<AtomicBool>,
    pub mic_test_active: bool,

    // Трей (Windows)
    pub show_close_dialog: bool,
    #[cfg(target_os = "windows")]
    pub tray_open_id: Option<tray_icon::menu::MenuId>,
    #[cfg(target_os = "windows")]
    pub tray_quit_id: Option<tray_icon::menu::MenuId>,

    // Чат
    pub chat_active_friend: Option<SavedFriend>,
    pub chat_messages: HashMap<String, Vec<TextMessage>>,
    pub chat_input: String,
}

impl App {
    pub fn new() -> Self {
        let cfg = load_config();
        let name_input = cfg.display_name.clone();
        let mut devs = list_audio_devs();
        devs.sel_in = cfg.selected_input.min(devs.inputs.len().saturating_sub(1));
        devs.sel_out = cfg.selected_output.min(devs.outputs.len().saturating_sub(1));

        Self {
            cfg,
            active_tab: Tab::Contacts,
            call_state: CallState::None,
            status: String::new(),
            is_connected: false,
            link_up: false,
            name_input,
            add_friend_input: String::new(),
            copied_code_banner: false,
            friend_statuses: HashMap::new(),
            pending_friend_requests: Vec::new(),
            my_peer_id: None,
            udp_port: 7879,
            net: None,
            net_server: String::new(),
            audio: None,
            ringtone: None,
            devs,
            call_start: None,
            mic_test_level: Arc::new(AtomicU8::new(0)),
            mic_test_stop: Arc::new(AtomicBool::new(false)),
            mic_test_active: false,
            show_close_dialog: false,
            #[cfg(target_os = "windows")]
            tray_open_id: None,
            #[cfg(target_os = "windows")]
            tray_quit_id: None,
            chat_active_friend: None,
            chat_messages: HashMap::new(),
            chat_input: String::new(),
        }
    }

    // ── Сеть ──

    pub fn send_msg(&self, msg: ControlMessage) {
        if let Some(net) = &self.net {
            let _ = net.outbox.send(msg);
        }
    }

    /// Запуск сетевого супервизора (однократно; переподключения — внутри net.rs)
    pub fn ensure_net(&mut self) {
        if self.net.is_some() {
            return;
        }
        let server = normalize_server(&self.cfg.server_address);
        self.net_server = server.clone();
        let creds = NetCredentials {
            server_addr: server,
            client_id: self.cfg.client_id.clone(),
            user_code: self.cfg.user_code.clone(),
            display_name: self.cfg.display_name.clone(),
            auth_token: self.cfg.auth_token.clone(),
            server_fingerprint: self.cfg.server_fingerprint.clone(),
            tls_enabled: self.cfg.tls_enabled,
        };
        self.net = Some(net_start(creds));
    }

    /// Перезапуск сети при смене адреса сервера или имени
    pub fn restart_net(&mut self) {
        self.is_connected = false;
        self.link_up = false;
        self.my_peer_id = None;
        self.stop_audio();
        self.call_state = CallState::None;
        self.net = None; // Drop остановит супервизор
        self.ensure_net();
    }

    pub fn connect_register(&mut self) {
        let name = self.name_input.trim().to_string();
        if name.is_empty() {
            self.status = "Введите имя!".into();
            return;
        }
        if self.cfg.display_name != name {
            self.cfg.display_name = name;
            save_config(&self.cfg);
            self.restart_net();
            return;
        }
        self.ensure_net();
    }

    // ── Друзья ──

    pub fn request_friends_status(&self) {
        let codes: Vec<String> = self.cfg.friends.iter().map(|f| f.user_code.clone()).collect();
        if !codes.is_empty() {
            self.send_msg(ControlMessage::GetFriendsStatus { user_codes: codes });
        }
    }

    pub fn send_friend_request(&mut self, code: &str) {
        let clean: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
        if clean.len() != 6 {
            self.status = "ID должен состоять из 6 цифр".into();
            return;
        }
        if clean == self.cfg.user_code {
            self.status = "Нельзя отправить запрос самому себе".into();
            return;
        }
        if self.cfg.friends.iter().any(|f| f.user_code == clean) {
            self.status = "Этот пользователь уже у вас в друзьях".into();
            return;
        }
        self.status = format!("Отправка запроса в друзья ID {}...", clean);
        self.send_msg(ControlMessage::SendFriendRequest { target_code: clean });
    }

    pub fn accept_friend_request(&mut self, from_code: String) {
        self.send_msg(ControlMessage::AcceptFriendRequest { from_code: from_code.clone() });
        self.pending_friend_requests.retain(|r| r.from_code != from_code);
    }

    pub fn reject_friend_request(&mut self, from_code: String) {
        self.send_msg(ControlMessage::RejectFriendRequest { from_code: from_code.clone() });
        self.pending_friend_requests.retain(|r| r.from_code != from_code);
    }

    pub fn remove_friend(&mut self, code: &str) {
        self.cfg.friends.retain(|f| f.user_code != code);
        self.friend_statuses.remove(code);
        save_config(&self.cfg);
        self.status = "Контакт удалён".into();
    }

    // ── Чат ──

    /// Открыть чат с пользователем по ID/имени (из любого места: контакты, история, звонок)
    pub fn open_chat(&mut self, code: String, name: String) {
        let canonical = self
            .cfg
            .friends
            .iter()
            .find(|f| f.user_code == code)
            .cloned()
            .unwrap_or(SavedFriend { user_code: code, name });
        self.chat_active_friend = Some(canonical);
    }

    pub fn send_text_message(&mut self, target_code: String, text: String) {
        let text_clean = text.trim().to_string();
        if text_clean.is_empty() {
            return;
        }
        let msg_id = Uuid::new_v4().to_string();
        let msg = TextMessage {
            id: msg_id.clone(),
            from_code: self.cfg.user_code.clone(),
            from_name: self.cfg.display_name.clone(),
            to_code: target_code.clone(),
            text: text_clean.clone(),
            timestamp: Utc::now().to_rfc3339(),
        };
        self.chat_messages.entry(target_code.clone()).or_default().push(msg);
        self.send_msg(ControlMessage::SendTextMessage {
            target_code,
            text: text_clean,
            message_id: msg_id,
        });
    }

    // ── Звонки ──

    pub fn call_user(&mut self, target_code: String, target_name: String) {
        if !matches!(self.call_state, CallState::None) {
            return; // уже в звонке
        }
        self.chat_active_friend = None; // звонок важнее открытого чата
        self.call_start = Some(Instant::now());
        self.call_state = CallState::Calling {
            target_code: target_code.clone(),
            target_name: target_name.clone(),
        };
        self.status = format!("Вызов {}...", target_name);
        self.send_msg(ControlMessage::CallRequest { target_code });
    }

    pub fn accept_call(&mut self, from_peer_id: u32, from_name: String) {
        self.call_start = Some(Instant::now());
        self.send_msg(ControlMessage::CallAccept { target_peer_id: from_peer_id });
        self.status = format!("Соединение с {}...", from_name);
    }

    pub fn reject_call(&mut self, from_peer_id: u32) {
        self.send_msg(ControlMessage::CallReject { target_peer_id: from_peer_id });
        let (peer_name, peer_code) = self.current_call_peer();
        self.push_history(peer_name, peer_code, CallDirection::Missed, 0);
        self.call_state = CallState::None;
        self.status = "Звонок отклонён".into();
    }

    /// Завершение звонка по инициативе пользователя — трогает ТОЛЬКО аудио.
    /// Сетевой поток продолжает жить (главный баг v2 устранён).
    pub fn end_call(&mut self) {
        let (peer_name, peer_code, duration) = match &self.call_state {
            CallState::InCall { peer_name, peer_code, started_at, .. } => {
                (peer_name.clone(), peer_code.clone(), started_at.elapsed().as_secs())
            }
            CallState::Calling { target_name, target_code } => {
                (target_name.clone(), target_code.clone(), 0)
            }
            _ => {
                self.stop_audio();
                self.call_state = CallState::None;
                return;
            }
        };
        self.stop_audio();
        self.push_history(peer_name, peer_code, CallDirection::Outgoing, duration);
        self.send_msg(ControlMessage::CallEnd);
        self.call_state = CallState::None;
        self.status = "Звонок завершён".into();
    }

    /// (имя, ID) текущего собеседника по состоянию звонка
    fn current_call_peer(&self) -> (String, String) {
        match &self.call_state {
            CallState::InCall { peer_name, peer_code, .. } => {
                (peer_name.clone(), peer_code.clone())
            }
            CallState::Calling { target_name, target_code } => {
                (target_name.clone(), target_code.clone())
            }
            CallState::IncomingCall { from_name, from_code, .. } => {
                (from_name.clone(), from_code.clone())
            }
            CallState::None => (String::new(), String::new()),
        }
    }

    /// Звонок завершён сервером/партнёром или обрывом связи
    fn on_call_terminated(&mut self, direction: CallDirection) {
        let (peer_name, peer_code) = self.current_call_peer();
        let dur = self.call_start.map(|s| s.elapsed().as_secs()).unwrap_or(0);
        let was_active = !matches!(self.call_state, CallState::None) && !peer_name.is_empty();
        self.stop_audio();
        if was_active {
            self.push_history(peer_name, peer_code, direction, dur);
        }
        self.call_state = CallState::None;
    }

    fn push_history(
        &mut self,
        peer_name: String,
        peer_code: String,
        direction: CallDirection,
        duration: u64,
    ) {
        self.cfg.call_history.insert(
            0,
            CallRecord {
                peer_name,
                peer_code,
                direction,
                timestamp: Utc::now().to_rfc3339(),
                duration_secs: duration,
            },
        );
        if self.cfg.call_history.len() > 200 {
            self.cfg.call_history.truncate(200);
        }
        save_config(&self.cfg);
    }

    fn stop_audio(&mut self) {
        self.audio = None; // Drop AudioHandle остановит потоки
        self.ringtone = None; // Drop RingtoneHandle остановит рингтон
    }

    fn start_audio(&mut self, call_id: u64) {
        let host = server_host(&self.cfg.server_address);
        let target = match format!("{}:{}", host, self.udp_port).parse() {
            Ok(a) => a,
            Err(e) => {
                self.status = format!("Неверный адрес сервера: {}", e);
                return;
            }
        };
        let sock = match StdUdpSocket::bind("0.0.0.0:0") {
            Ok(s) => Arc::new(s),
            Err(e) => {
                self.status = format!("UDP ошибка: {}", e);
                return;
            }
        };
        let cfg = CallAudioConfig {
            input_device: self.devs.selected_input_name(),
            output_device: self.devs.selected_output_name(),
            sock,
            target,
            call_id,
            my_peer_id: self.my_peer_id.unwrap_or(0),
        };
        let audio_handle = start_call_audio(cfg);
        audio_handle.mic_gain.store(self.cfg.mic_gain.to_bits(), Ordering::Relaxed);
        self.audio = Some(audio_handle);
    }

    // ── Управление звуком в звонке (через атомики AudioHandle) ──

    pub fn set_mic_gain(&mut self, g: f32) {
        self.cfg.mic_gain = g;
        if let Some(a) = &self.audio {
            a.mic_gain.store(g.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn mic_muted(&self) -> bool {
        self.audio.as_ref().map(|a| a.mic_muted.load(Ordering::Relaxed)).unwrap_or(false)
    }

    pub fn toggle_mic(&self) {
        if let Some(a) = &self.audio {
            a.mic_muted.fetch_xor(true, Ordering::Relaxed);
        }
    }

    pub fn sound_muted(&self) -> bool {
        self.audio.as_ref().map(|a| a.output_muted.load(Ordering::Relaxed)).unwrap_or(false)
    }

    pub fn toggle_sound(&self) {
        if let Some(a) = &self.audio {
            a.output_muted.fetch_xor(true, Ordering::Relaxed);
        }
    }

    pub fn mic_level(&self) -> u8 {
        self.audio.as_ref().map(|a| a.mic_level.load(Ordering::Relaxed)).unwrap_or(0)
    }

    pub fn peer_volume(&self) -> f32 {
        self.audio
            .as_ref()
            .map(|a| f32::from_bits(a.peer_volume.load(Ordering::Relaxed)))
            .unwrap_or(1.0)
    }

    pub fn set_peer_volume(&self, v: f32) {
        if let Some(a) = &self.audio {
            a.peer_volume.store(v.to_bits(), Ordering::Relaxed);
        }
    }

    pub fn pkts_sent(&self) -> u64 {
        self.audio
            .as_ref()
            .map(|a| a.stats.pkts_sent.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn pkts_recv(&self) -> u64 {
        self.audio
            .as_ref()
            .map(|a| a.stats.pkts_recv.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Сколько раз кольцо воспроизведения пустовало (диагностика «скрипов»)
    pub fn audio_underruns(&self) -> u64 {
        self.audio
            .as_ref()
            .map(|a| a.stats.underruns.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn audio_error(&self) -> Option<String> {
        self.audio
            .as_ref()
            .and_then(|a| a.error.lock().ok().and_then(|e| e.clone()))
    }

    // ── Тест микрофона ──

    pub fn start_mic_test(&mut self) {
        self.mic_test_stop.store(false, Ordering::SeqCst);
        self.mic_test_active = true;
        let dev_name = self.devs.selected_input_name().unwrap_or_default();
        let level = self.mic_test_level.clone();
        let stop = self.mic_test_stop.clone();
        thread::spawn(move || {
            if let Err(e) = run_mic_test(dev_name, level.clone(), stop) {
                error!("Тест микрофона: {:?}", e);
                level.store(0, Ordering::Relaxed);
            }
        });
    }

    pub fn stop_mic_test(&mut self) {
        self.mic_test_stop.store(true, Ordering::SeqCst);
        self.mic_test_active = false;
    }

    // ── Обработка событий ──

    pub fn poll(&mut self) {
        let mut evs = Vec::new();
        if let Some(net) = &self.net {
            while let Ok(e) = net.events_rx.try_recv() {
                evs.push(e);
            }
        }
        for ev in evs {
            match ev {
                NetEvent::LinkUp => {
                    self.link_up = true;
                    self.status = "Подключение к серверу...".into();
                }
                NetEvent::LinkDown => {
                    self.link_up = false;
                    if self.is_connected {
                        self.status = "Соединение потеряно. Переподключение...".into();
                    } else {
                        self.status = "Нет связи с сервером. Повторная попытка...".into();
                    }
                    self.is_connected = false;
                    self.on_call_terminated(CallDirection::Missed);
                }
                NetEvent::Msg(msg) => self.handle_server_msg(msg),
                NetEvent::TlsFingerprintDiscovered(fp) => {
                    if self.cfg.server_fingerprint.as_ref() != Some(&fp) {
                        tracing::info!("Pinning discovered TLS fingerprint: {}", fp);
                        self.cfg.server_fingerprint = Some(fp);
                        save_config(&self.cfg);
                    }
                }
            }
        }
    }

    fn handle_server_msg(&mut self, msg: ControlMessage) {
        match msg {
            ControlMessage::Registered { peer_id, user_code, auth_token, udp_port } => {
                self.my_peer_id = Some(peer_id);
                self.udp_port = udp_port;
                if self.cfg.user_code != user_code {
                    self.cfg.user_code = user_code.clone();
                }
                if let Some(token) = auth_token {
                    self.cfg.auth_token = Some(token);
                }
                save_config(&self.cfg);
                self.is_connected = true;
                self.link_up = true;
                self.status =
                    format!("В сети как {} (ID: {})", self.cfg.display_name, user_code);
                self.request_friends_status();
            }
            ControlMessage::AuthResponse { outcome } => {
                use cheburgram_protocol::{AuthError, AuthOutcome};
                match outcome {
                    AuthOutcome::Ok { peer_id, user_code, udp_port, auth_token } => {
                        self.my_peer_id = Some(peer_id);
                        self.udp_port = udp_port;
                        if self.cfg.user_code != user_code {
                            self.cfg.user_code = user_code.clone();
                        }
                        if let Some(token) = auth_token {
                            self.cfg.auth_token = Some(token);
                        }
                        save_config(&self.cfg);
                        self.is_connected = true;
                        self.link_up = true;
                        self.status =
                            format!("В сети как {} (ID: {})", self.cfg.display_name, self.cfg.user_code);
                        self.request_friends_status();
                    }
                    AuthOutcome::Failed(err) => {
                        self.is_connected = false;
                        match err {
                            AuthError::InvalidToken | AuthError::UnknownAccount => {
                                self.cfg.auth_token = None;
                                save_config(&self.cfg);
                                self.status =
                                    "Аккаунт не найден или токен недействителен. Зарегистрируйтесь заново.".into();
                            }
                            AuthError::RateLimited { retry_after_secs } => {
                                self.status = format!(
                                    "Слишком много попыток входа. Попробуйте через {} с.",
                                    retry_after_secs
                                );
                            }
                            AuthError::ProtocolViolation => {
                                self.status = "Ошибка протокола авторизации".into();
                            }
                        }
                    }
                }
            }
            ControlMessage::SessionReplaced => {
                self.is_connected = false;
                self.status =
                    "Этот аккаунт вошёл с другого устройства. Переподключение...".into();
                self.on_call_terminated(CallDirection::Missed);
            }
            ControlMessage::VersionMismatch { min, max } => {
                self.status = format!(
                    "Версия протокола устарела (сервер: {}-{}). Обновите приложение!",
                    min, max
                );
            }
            ControlMessage::IncomingFriendRequest { from_code, from_name } => {
                if !self.pending_friend_requests.iter().any(|r| r.from_code == from_code) {
                    self.pending_friend_requests
                        .push(FriendRequestInfo { from_code, from_name });
                }
            }
            ControlMessage::PendingFriendRequests { requests } => {
                self.pending_friend_requests = requests;
            }
            ControlMessage::FriendRequestAccepted { user_code, name } => {
                let friend = SavedFriend { user_code: user_code.clone(), name: name.clone() };
                if !self.cfg.friends.contains(&friend) {
                    self.cfg.friends.push(friend);
                    save_config(&self.cfg);
                }
                self.status = format!("{} добавил(а) вас в друзья!", name);
                self.request_friends_status();
            }
            ControlMessage::FriendsStatus { friends } => {
                for f in friends {
                    self.friend_statuses.insert(f.user_code.clone(), f);
                }
            }
            ControlMessage::IncomingTextMessage { msg } => {
                self.chat_messages.entry(msg.from_code.clone()).or_default().push(msg);
            }
            ControlMessage::PendingTextMessages { messages } => {
                for m in messages {
                    self.chat_messages.entry(m.from_code.clone()).or_default().push(m);
                }
            }
            ControlMessage::UserStatusChanged { user_code, is_online, peer_id } => {
                if let Some(f) = self.friend_statuses.get_mut(&user_code) {
                    f.is_online = is_online;
                    f.peer_id = peer_id;
                } else if self.cfg.friends.iter().any(|fr| fr.user_code == user_code) {
                    self.request_friends_status();
                }
            }
            ControlMessage::IncomingCall { from_code, from_name, from_peer_id } => {
                if matches!(self.call_state, CallState::None) {
                    self.call_state =
                        CallState::IncomingCall { from_code, from_name, from_peer_id };
                    self.ringtone = Some(cheburgram_audio::start_ringtone(
                        Some("call.mp3"),
                        self.devs.selected_output_name(),
                    ));
                }
            }
            ControlMessage::CallAccepted { peer_id, peer_name, call_id } => {
                // ID собеседника берём из предыдущего состояния (Calling/IncomingCall),
                // fallback — поиск по peer_id среди друзей
                let peer_code = match &self.call_state {
                    CallState::Calling { target_code, .. } => target_code.clone(),
                    CallState::IncomingCall { from_code, .. } => from_code.clone(),
                    _ => self
                        .friend_statuses
                        .iter()
                        .find(|(_, f)| f.peer_id == Some(peer_id))
                        .map(|(c, _)| c.clone())
                        .unwrap_or_default(),
                };
                self.start_audio(call_id);
                let started = self.call_start.unwrap_or_else(Instant::now);
                self.call_state = CallState::InCall {
                    peer_id,
                    peer_name: peer_name.clone(),
                    peer_code,
                    call_id,
                    started_at: started,
                };
                self.status = format!("В разговоре с {}", peer_name);
            }
            ControlMessage::CallRejected { peer_name, .. } => {
                let (_, peer_code) = self.current_call_peer();
                self.call_state = CallState::None;
                self.status = format!("{} недоступен(на)", peer_name);
                self.push_history(peer_name, peer_code, CallDirection::Outgoing, 0);
            }
            ControlMessage::CallEnded { peer_name } => {
                let dir = match &self.call_state {
                    CallState::InCall { .. } => CallDirection::Incoming,
                    _ => CallDirection::Missed,
                };
                self.on_call_terminated(dir);
                self.status = format!("{} завершил(а) звонок", peer_name);
            }
            ControlMessage::CallMissed { peer_name: _ } => {
                self.on_call_terminated(CallDirection::Missed);
                self.status = "Пропущенный звонок".into();
            }
            ControlMessage::Error { message } => {
                self.status = format!("ℹ {}", message);
                if matches!(self.call_state, CallState::Calling { .. }) {
                    self.call_state = CallState::None;
                }
            }
            _ => {}
        }
    }
}

// ─── Тест микрофона (вне звонка) ─────────────────────────────────────────────

fn run_mic_test(
    dev_name: String,
    level: Arc<AtomicU8>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let device = cheburgram_audio::devices::open_input(
        if dev_name.is_empty() { None } else { Some(dev_name.as_str()) },
    )
    .ok_or_else(|| anyhow::anyhow!("нет микрофона"))?;
    let cfg_d = device.default_input_config()?;
    let fmt = cfg_d.sample_format();
    let config: cpal::StreamConfig = cfg_d.into();
    let ch = config.channels as usize;

    let buf = Arc::new(Mutex::new(Vec::<f32>::new()));
    let buf_c = buf.clone();
    let err_fn = |e: cpal::StreamError| error!("mic test: {}", e);
    let stream = match fmt {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                let mut b = buf_c.lock().unwrap();
                if ch == 1 {
                    b.extend_from_slice(data);
                } else {
                    for frame in data.chunks(ch) {
                        b.push(frame.iter().sum::<f32>() / ch as f32);
                    }
                }
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let mut b = buf_c.lock().unwrap();
                if ch == 1 {
                    b.extend(data.iter().map(|&s| s as f32 / 32768.0));
                } else {
                    for frame in data.chunks(ch) {
                        b.push(frame.iter().map(|&s| s as f32 / 32768.0).sum::<f32>() / ch as f32);
                    }
                }
            },
            err_fn,
            None,
        )?,
        other => anyhow::bail!("формат {:?} не поддерживается", other),
    };
    stream.play()?;

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(30));
        let samples: Vec<f32> = {
            let mut b = buf.lock().unwrap();
            std::mem::take(&mut *b)
        };
        if !samples.is_empty() {
            let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
            level.store(((rms * 6.0).sqrt() * 100.0).min(100.0) as u8, Ordering::Relaxed);
        }
    }
    level.store(0, Ordering::Relaxed);
    Ok(())
}

//! Сетевой слой клиента: TCP-сигналинг с автопереподключением.
//!
//! Ключевые свойства (исправления v2):
//! - собственный stop-флаг, НЕ связанный с аудио — конец звонка не убивает сеть
//! - reconnect с экспоненциальным backoff (1с → 30с) при любом обрыве
//! - heartbeat Ping каждые 15 с (сервер ждёт 60 с)
//! - UI общается через каналы: outbox (команды) / events (события)

use cheburgram_protocol::{
    read_frame_sync, write_frame_sync, ControlMessage, PROTOCOL_VERSION,
};
use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
const PING_INTERVAL: Duration = Duration::from_secs(15);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// События сетевого слоя для UI
#[derive(Debug)]
pub enum NetEvent {
    /// TCP установлено, Hello+Register отправлены (ждём Registered)
    LinkUp,
    /// Соединение потеряно или не удалось; супервизор уже переподключается
    LinkDown,
    /// Сообщение от сервера
    Msg(ControlMessage),
}

pub struct NetHandle {
    pub events_rx: Receiver<NetEvent>,
    pub outbox: Sender<ControlMessage>,
    pub stop: Arc<AtomicBool>,
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub struct NetCredentials {
    pub server_addr: String,
    pub client_id: String,
    pub user_code: String,
    pub display_name: String,
}

/// Запуск супервизора соединения. Живёт до stop=true.
pub fn start(creds: NetCredentials) -> NetHandle {
    let (events_tx, events_rx) = channel::<NetEvent>();
    let (outbox_tx, outbox_rx) = channel::<ControlMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    {
        let stop = stop.clone();
        let events = events_tx.clone();
        thread::spawn(move || supervisor(creds, outbox_rx, events, stop));
    }

    NetHandle {
        events_rx,
        outbox: outbox_tx,
        stop,
    }
}

fn supervisor(
    creds: NetCredentials,
    outbox: Receiver<ControlMessage>,
    events: Sender<NetEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = BACKOFF_MIN;

    while !stop.load(Ordering::SeqCst) {
        // резолв адреса (IP или домен)
        let addr = match creds.server_addr.parse() {
            Ok(a) => Some(a),
            Err(_) => std::net::ToSocketAddrs::to_socket_addrs(&creds.server_addr.as_str())
                .ok()
                .and_then(|mut it| it.next()),
        };
        let Some(addr) = addr else {
            warn!("Не резолвится адрес сервера: {}", creds.server_addr);
            interruptible_sleep(backoff, &stop);
            backoff = (backoff * 2).min(BACKOFF_MAX);
            continue;
        };

        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                backoff = BACKOFF_MIN;
                info!("TCP подключено к {}", creds.server_addr);
                run_session(&stream, &creds, &outbox, &events, &stop);
                let _ = events.send(NetEvent::LinkDown);
                if !stop.load(Ordering::SeqCst) {
                    info!("Сессия завершена, переподключение через {:?}", backoff);
                }
            }
            Err(e) => {
                warn!("Подключение не удалось: {}", e);
                let _ = events.send(NetEvent::LinkDown);
            }
        }
        interruptible_sleep(backoff, &stop);
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Одна TCP-сессия: читает входящие в потоке-слушателе, пишет из outbox + Ping
fn run_session(
    stream: &TcpStream,
    creds: &NetCredentials,
    outbox: &Receiver<ControlMessage>,
    events: &Sender<NetEvent>,
    stop: &Arc<AtomicBool>,
) {
    let _ = stream.set_nodelay(true);
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            warn!("try_clone: {}", e);
            return;
        }
    };

    // рукопожатие + регистрация
    if write_frame_sync(&mut writer, &ControlMessage::Hello { protocol_version: PROTOCOL_VERSION })
        .is_err()
    {
        return;
    }
    let register = ControlMessage::Register {
        client_id: creds.client_id.clone(),
        user_code: creds.user_code.clone(),
        name: creds.display_name.clone(),
    };
    if write_frame_sync(&mut writer, &register).is_err() {
        return;
    }

    // поток чтения
    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let reader_alive = Arc::new(AtomicBool::new(true));
    {
        let reader_alive = reader_alive.clone();
        let events = events.clone();
        thread::spawn(move || {
            let mut reader = io::BufReader::new(reader_stream);
            loop {
                match read_frame_sync(&mut reader) {
                    Ok(msg) => {
                        if events.send(NetEvent::Msg(msg)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            reader_alive.store(false, Ordering::SeqCst);
        });
    }

    let _ = events.send(NetEvent::LinkUp);

    // цикл записи: outbox + heartbeat
    let mut last_ping = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) || !reader_alive.load(Ordering::Relaxed) {
            break;
        }
        while let Ok(msg) = outbox.try_recv() {
            if write_frame_sync(&mut writer, &msg).is_err() {
                return;
            }
        }
        if last_ping.elapsed() >= PING_INTERVAL {
            last_ping = Instant::now();
            if write_frame_sync(&mut writer, &ControlMessage::Ping).is_err() {
                return;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Сон с проверкой stop каждые 100 мс
fn interruptible_sleep(d: Duration, stop: &Arc<AtomicBool>) {
    let started = Instant::now();
    while started.elapsed() < d {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

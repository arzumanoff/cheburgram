use anyhow::{Context, Result};
use cheburgram_protocol::{AudioPacket, ControlMessage, UserInfo};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, Mutex},
};
use tracing::{error, info, warn};

const TCP_SIGNAL_PORT: u16 = 7878;
const UDP_MEDIA_PORT: u16 = 7879;
const CLIENTS_FILE: &str = "clients.json";

// ─── Постоянный реестр клиентов (хранится на диске) ───────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ClientRegistry {
    /// UUID клиента → запись
    clients: HashMap<String, RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryEntry {
    name: String,
    last_seen: String,
}

impl ClientRegistry {
    fn load() -> Self {
        if let Ok(data) = std::fs::read_to_string(CLIENTS_FILE) {
            if let Ok(reg) = serde_json::from_str(&data) {
                return reg;
            }
        }
        Self::default()
    }

    fn save(&self) {
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(CLIENTS_FILE, data);
        }
    }

    fn upsert(&mut self, client_id: &str, name: &str) {
        let now = chrono::Utc::now().to_rfc3339();
        self.clients.insert(
            client_id.to_string(),
            RegistryEntry { name: name.to_string(), last_seen: now },
        );
        self.save();
    }
}

// ─── Онлайн-состояние (в памяти) ─────────────────────────────────────────────

#[derive(Debug)]
struct OnlineUser {
    peer_id: u32,
    client_id: String,
    name: String,
    tx: mpsc::UnboundedSender<ControlMessage>,
    udp_addr: Option<SocketAddr>,
    /// С кем сейчас в звонке (peer_id)
    in_call_with: Option<u32>,
}

#[derive(Debug, Default)]
struct State {
    /// peer_id → OnlineUser
    online: HashMap<u32, OnlineUser>,
    next_peer_id: u32,
    /// Текущий ID сессии звонка (для UDP реле)
    next_call_id: u64,
    /// call_session_id → (peer_id_a, peer_id_b)
    active_calls: HashMap<u64, (u32, u32)>,
}

impl State {
    fn user_list(&self) -> Vec<UserInfo> {
        self.online
            .values()
            .map(|u| UserInfo { peer_id: u.peer_id, name: u.name.clone() })
            .collect()
    }

    fn broadcast_except(&self, except_peer: u32, msg: ControlMessage) {
        for (&pid, user) in &self.online {
            if pid != except_peer {
                let _ = user.tx.send(msg.clone());
            }
        }
    }
}

type SharedState = Arc<Mutex<State>>;

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("🚀 Запуск Cheburgram Server v2...");
    info!("   TCP сигналы: 0.0.0.0:{}", TCP_SIGNAL_PORT);
    info!("   UDP медиа:   0.0.0.0:{}", UDP_MEDIA_PORT);

    let registry = Arc::new(Mutex::new(ClientRegistry::load()));
    info!("📋 Загружен реестр: {} клиентов", registry.lock().await.clients.len());

    let state: SharedState = Arc::new(Mutex::new(State::default()));

    // ── UDP Медиа-реле ────────────────────────────────────────────────────────
    let udp_socket = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{}", UDP_MEDIA_PORT))
            .await
            .context("Не удалось привязать UDP")?,
    );

    let state_udp = state.clone();
    let udp_recv = udp_socket.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            match udp_recv.recv_from(&mut buf).await {
                Ok((len, src_addr)) => {
                    let data = &buf[..len];
                    if let Ok(packet) = AudioPacket::from_bytes(data) {
                        let mut st = state_udp.lock().await;

                        // Регистрируем UDP адрес
                        if let Some(user) = st.online.values_mut()
                            .find(|u| u.peer_id == packet.header.sender_id)
                        {
                            if user.udp_addr != Some(src_addr) {
                                info!("📍 UDP addr: peer={} → {}", packet.header.sender_id, src_addr);
                                user.udp_addr = Some(src_addr);
                            }
                        }

                        // Пустой payload = только регистрация, не пересылаем
                        if packet.payload.is_empty() {
                            continue;
                        }

                        // Найти звонок и переслать партнёру
                        let sender_id = packet.header.sender_id;
                        let call_id = packet.header.room_id;
                        if let Some(&(a, b)) = st.active_calls.get(&call_id) {
                            let target_id = if a == sender_id { b } else { a };
                            if let Some(target) = st.online.get(&target_id) {
                                if let Some(target_addr) = target.udp_addr {
                                    let udp_send = udp_recv.clone();
                                    let pkt = data.to_vec();
                                    tokio::spawn(async move {
                                        let _ = udp_send.send_to(&pkt, target_addr).await;
                                    });
                                }
                            }
                        }
                    }
                }
                Err(e) => error!("UDP ошибка: {}", e),
            }
        }
    });

    // ── TCP Сигнальный сервер ────────────────────────────────────────────────
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", TCP_SIGNAL_PORT))
        .await
        .context("Не удалось привязать TCP")?;

    loop {
        let (stream, peer_addr) = tcp_listener.accept().await?;
        info!("🔌 TCP подключение: {}", peer_addr);
        let state_c = state.clone();
        let registry_c = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, state_c, registry_c).await {
                warn!("Клиент {} отключился: {:?}", peer_addr, e);
            }
        });
    }
}

// ─── Обработка клиента ────────────────────────────────────────────────────────

async fn handle_client(
    stream: TcpStream,
    state: SharedState,
    registry: Arc<Mutex<ClientRegistry>>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let (tx, mut rx) = mpsc::unbounded_channel::<ControlMessage>();

    // Задача отправки
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if writer.write_all(format!("{}\n", json).as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });

    let mut my_peer_id: Option<u32> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        let msg: ControlMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            ControlMessage::Register { client_id, name } => {
                let mut st = state.lock().await;
                let peer_id = st.next_peer_id;
                st.next_peer_id += 1;

                info!("✅ Зарегистрирован: '{}' (peer={}, uuid={}...)", name, peer_id, &client_id[..8]);

                // Обновляем реестр на диске
                {
                    let mut reg = registry.lock().await;
                    reg.upsert(&client_id, &name);
                }

                let user_list = st.user_list();

                let user = OnlineUser {
                    peer_id,
                    client_id,
                    name: name.clone(),
                    tx: tx.clone(),
                    udp_addr: None,
                    in_call_with: None,
                };
                st.online.insert(peer_id, user);
                my_peer_id = Some(peer_id);

                // Отправляем новому: подтверждение + список онлайн
                let _ = tx.send(ControlMessage::Registered {
                    peer_id,
                    udp_port: UDP_MEDIA_PORT,
                });
                let _ = tx.send(ControlMessage::UserList { users: user_list });

                // Всем остальным: новый юзер онлайн
                st.broadcast_except(peer_id, ControlMessage::UserOnline {
                    peer_id,
                    name,
                });
            }

            ControlMessage::CallRequest { to_id } => {
                let st = state.lock().await;
                let from_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };
                let from_name = st.online.get(&from_id)
                    .map(|u| u.name.clone())
                    .unwrap_or_default();

                if let Some(target) = st.online.get(&to_id) {
                    if target.in_call_with.is_some() {
                        let _ = tx.send(ControlMessage::Error {
                            message: format!("{} сейчас занят", target.name),
                        });
                    } else {
                        info!("📞 Звонок: peer={} → peer={}", from_id, to_id);
                        let _ = target.tx.send(ControlMessage::IncomingCall {
                            from_id,
                            from_name,
                        });
                    }
                } else {
                    let _ = tx.send(ControlMessage::Error {
                        message: "Пользователь недоступен".to_string(),
                    });
                }
            }

            ControlMessage::CallAccept { to_id } => {
                let mut st = state.lock().await;
                let from_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };

                let call_id = st.next_call_id;
                st.next_call_id += 1;
                st.active_calls.insert(call_id, (from_id, to_id));

                let (from_name, to_name) = {
                    let fn_ = st.online.get(&from_id).map(|u| u.name.clone()).unwrap_or_default();
                    let tn = st.online.get(&to_id).map(|u| u.name.clone()).unwrap_or_default();
                    (fn_, tn)
                };

                // Помечаем обоих как "в звонке"
                if let Some(u) = st.online.get_mut(&from_id) {
                    u.in_call_with = Some(to_id);
                }
                if let Some(u) = st.online.get_mut(&to_id) {
                    u.in_call_with = Some(from_id);
                }

                info!("✅ Звонок начат: peer={} ↔ peer={} (call_id={})", from_id, to_id, call_id);

                // Уведомляем инициатора
                if let Some(initiator) = st.online.get(&to_id) {
                    let _ = initiator.tx.send(ControlMessage::CallAccepted {
                        peer_id: from_id,
                        peer_name: from_name,
                    });
                }
                // Принявшему — тоже CallAccepted
                let _ = tx.send(ControlMessage::CallAccepted {
                    peer_id: to_id,
                    peer_name: to_name,
                });
            }

            ControlMessage::CallReject { to_id } => {
                let st = state.lock().await;
                let from_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };
                let from_name = st.online.get(&from_id)
                    .map(|u| u.name.clone())
                    .unwrap_or_default();

                if let Some(target) = st.online.get(&to_id) {
                    let _ = target.tx.send(ControlMessage::CallRejected {
                        peer_id: from_id,
                        peer_name: from_name,
                    });
                }
                info!("❌ Звонок отклонён: peer={} → peer={}", from_id, to_id);
            }

            ControlMessage::CallEnd => {
                let mut st = state.lock().await;
                let my_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };

                let partner_id = st.online.get(&my_id)
                    .and_then(|u| u.in_call_with);
                let my_name = st.online.get(&my_id)
                    .map(|u| u.name.clone())
                    .unwrap_or_default();

                // Убираем пометку "в звонке"
                if let Some(u) = st.online.get_mut(&my_id) {
                    u.in_call_with = None;
                }

                // Удаляем активный звонок
                st.active_calls.retain(|_, &mut (a, b)| a != my_id && b != my_id);

                if let Some(pid) = partner_id {
                    if let Some(u) = st.online.get_mut(&pid) {
                        u.in_call_with = None;
                    }
                    if let Some(partner) = st.online.get(&pid) {
                        let _ = partner.tx.send(ControlMessage::CallEnded {
                            peer_name: my_name,
                        });
                    }
                    info!("📴 Звонок завершён: peer={} ↔ peer={}", my_id, pid);
                }
            }

            ControlMessage::Ping => {
                let _ = tx.send(ControlMessage::Pong);
            }

            _ => {}
        }
    }

    // Отключение
    if let Some(my_id) = my_peer_id {
        let mut st = state.lock().await;

        let partner_id = st.online.get(&my_id).and_then(|u| u.in_call_with);
        let my_name = st.online.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();

        // Завершить звонок если был
        if let Some(pid) = partner_id {
            if let Some(u) = st.online.get_mut(&pid) {
                u.in_call_with = None;
            }
            if let Some(partner) = st.online.get(&pid) {
                let _ = partner.tx.send(ControlMessage::CallEnded {
                    peer_name: my_name.clone(),
                });
            }
            st.active_calls.retain(|_, &mut (a, b)| a != my_id && b != my_id);
        }

        st.online.remove(&my_id);
        info!("👋 Офлайн: '{}' (peer={})", my_name, my_id);
        st.broadcast_except(my_id, ControlMessage::UserOffline {
            peer_id: my_id,
            name: my_name,
        });
    }

    send_task.abort();
    Ok(())
}

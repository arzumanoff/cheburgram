use anyhow::{Context, Result};
use cheburgram_protocol::{AudioPacket, ControlMessage, FriendStatus};
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

// ─── Постоянный реестр клиентов (на диске VPS) ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ClientRegistry {
    /// user_code (6 цифр) → запись
    clients: HashMap<String, RegistryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryEntry {
    client_id: String,
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

    fn generate_code(&self) -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        loop {
            let code = format!("{:06}", rng.gen_range(100_000..999_999));
            if !self.clients.contains_key(&code) {
                return code;
            }
        }
    }

    fn upsert(&mut self, user_code: &str, client_id: &str, name: &str) -> String {
        let code = if user_code.len() == 6 && user_code.chars().all(|c| c.is_ascii_digit()) {
            user_code.to_string()
        } else {
            // Ищем по client_id
            if let Some((existing_code, _)) = self.clients.iter().find(|(_, v)| v.client_id == client_id) {
                existing_code.clone()
            } else {
                self.generate_code()
            }
        };

        let now = chrono::Utc::now().to_rfc3339();
        self.clients.insert(
            code.clone(),
            RegistryEntry {
                client_id: client_id.to_string(),
                name: name.to_string(),
                last_seen: now,
            },
        );
        self.save();
        code
    }
}

// ─── Состояние онлайн ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct OnlineUser {
    peer_id: u32,
    user_code: String,
    client_id: String,
    name: String,
    tx: mpsc::UnboundedSender<ControlMessage>,
    udp_addr: Option<SocketAddr>,
    in_call_with: Option<u32>,
}

#[derive(Debug, Default)]
struct State {
    /// peer_id → OnlineUser
    online_by_peer: HashMap<u32, OnlineUser>,
    /// user_code → peer_id
    online_by_code: HashMap<String, u32>,

    next_peer_id: u32,
    next_call_id: u64,
    /// call_id → (peer_id_a, peer_id_b)
    active_calls: HashMap<u64, (u32, u32)>,
}

impl State {
    fn broadcast_status(&self, user_code: &str, is_online: bool, peer_id: Option<u32>) {
        let msg = ControlMessage::UserStatusChanged {
            user_code: user_code.to_string(),
            is_online,
            peer_id,
        };
        for user in self.online_by_peer.values() {
            let _ = user.tx.send(msg.clone());
        }
    }
}

type SharedState = Arc<Mutex<State>>;

// ─── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("🚀 Запуск Cheburgram Server v2.2 (ID/Друзья)...");
    info!("   TCP сигналы: 0.0.0.0:{}", TCP_SIGNAL_PORT);
    info!("   UDP медиа:   0.0.0.0:{}", UDP_MEDIA_PORT);

    let registry = Arc::new(Mutex::new(ClientRegistry::load()));
    info!("📋 Загружен реестр: {} пользователей", registry.lock().await.clients.len());

    let state: SharedState = Arc::new(Mutex::new(State::default()));

    // UDP Реле
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

                        if let Some(user) = st.online_by_peer.get_mut(&packet.header.sender_id) {
                            if user.udp_addr != Some(src_addr) {
                                info!("📍 UDP registered: peer={} ({}) -> {}", packet.header.sender_id, user.name, src_addr);
                                user.udp_addr = Some(src_addr);
                            }
                        }

                        if packet.payload.is_empty() {
                            continue;
                        }

                        let sender_id = packet.header.sender_id;
                        let call_id = packet.header.room_id;
                        if let Some(&(a, b)) = st.active_calls.get(&call_id) {
                            let target_id = if a == sender_id { b } else { a };
                            if let Some(target) = st.online_by_peer.get(&target_id) {
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
                Err(e) => error!("UDP error: {}", e),
            }
        }
    });

    // TCP Сигналы
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", TCP_SIGNAL_PORT))
        .await
        .context("Не удалось привязать TCP")?;

    loop {
        let (stream, peer_addr) = tcp_listener.accept().await?;
        info!("🔌 TCP connected: {}", peer_addr);
        let state_c = state.clone();
        let registry_c = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, state_c, registry_c).await {
                warn!("Client {} disconnected: {:?}", peer_addr, e);
            }
        });
    }
}

async fn handle_client(
    stream: TcpStream,
    state: SharedState,
    registry: Arc<Mutex<ClientRegistry>>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let (tx, mut rx) = mpsc::unbounded_channel::<ControlMessage>();

    // Отправка сообщений клиенту по ЕДИНОМУ TCP каналу
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
    let mut my_user_code: Option<String> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        let msg: ControlMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            ControlMessage::Register { client_id, user_code, name } => {
                let code = {
                    let mut reg = registry.lock().await;
                    reg.upsert(&user_code, &client_id, &name)
                };

                let mut st = state.lock().await;
                let peer_id = st.next_peer_id;
                st.next_peer_id += 1;

                info!("✅ Registered: '{}' (ID: {}, peer={})", name, code, peer_id);

                let user = OnlineUser {
                    peer_id,
                    user_code: code.clone(),
                    client_id,
                    name: name.clone(),
                    tx: tx.clone(),
                    udp_addr: None,
                    in_call_with: None,
                };

                st.online_by_code.insert(code.clone(), peer_id);
                st.online_by_peer.insert(peer_id, user);

                my_peer_id = Some(peer_id);
                my_user_code = Some(code.clone());

                let _ = tx.send(ControlMessage::Registered {
                    peer_id,
                    user_code: code.clone(),
                    udp_port: UDP_MEDIA_PORT,
                });

                st.broadcast_status(&code, true, Some(peer_id));
            }

            ControlMessage::LookupUser { user_code } => {
                let reg = registry.lock().await;
                let code_clean = user_code.trim().to_string();
                if let Some(entry) = reg.clients.get(&code_clean) {
                    let st = state.lock().await;
                    let (is_online, peer_id) = match st.online_by_code.get(&code_clean) {
                        Some(&pid) => (true, Some(pid)),
                        None => (false, None),
                    };
                    let _ = tx.send(ControlMessage::UserLookupResult {
                        found: true,
                        user_code: code_clean,
                        name: entry.name.clone(),
                        is_online,
                        peer_id,
                    });
                } else {
                    let _ = tx.send(ControlMessage::UserLookupResult {
                        found: false,
                        user_code: code_clean,
                        name: String::new(),
                        is_online: false,
                        peer_id: None,
                    });
                }
            }

            ControlMessage::GetFriendsStatus { user_codes } => {
                let reg = registry.lock().await;
                let st = state.lock().await;
                let mut friends = Vec::new();

                for code in user_codes {
                    let code_clean = code.trim().to_string();
                    if let Some(entry) = reg.clients.get(&code_clean) {
                        let (is_online, peer_id) = match st.online_by_code.get(&code_clean) {
                            Some(&pid) => (true, Some(pid)),
                            None => (false, None),
                        };
                        friends.push(FriendStatus {
                            user_code: code_clean,
                            name: entry.name.clone(),
                            is_online,
                            peer_id,
                        });
                    }
                }
                let _ = tx.send(ControlMessage::FriendsStatus { friends });
            }

            ControlMessage::CallRequest { target_code } => {
                let st = state.lock().await;
                let from_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };
                let from_code = my_user_code.clone().unwrap_or_default();
                let from_name = st.online_by_peer.get(&from_id)
                    .map(|u| u.name.clone())
                    .unwrap_or_default();

                let target_peer = st.online_by_code.get(&target_code.trim().to_string()).copied();

                if let Some(to_id) = target_peer {
                    if let Some(target) = st.online_by_peer.get(&to_id) {
                        if target.in_call_with.is_some() {
                            let _ = tx.send(ControlMessage::Error {
                                message: format!("{} сейчас занят(а)", target.name),
                            });
                        } else {
                            info!("📞 Call request: {} ({}) -> {} ({})", from_name, from_id, target.name, to_id);
                            let _ = target.tx.send(ControlMessage::IncomingCall {
                                from_code,
                                from_name,
                                from_peer_id: from_id,
                            });
                        }
                    }
                } else {
                    let _ = tx.send(ControlMessage::Error {
                        message: "Пользователь не в сети".to_string(),
                    });
                }
            }

            ControlMessage::CallAccept { target_peer_id } => {
                let mut st = state.lock().await;
                let my_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };

                let call_id = st.next_call_id;
                st.next_call_id += 1;
                st.active_calls.insert(call_id, (my_id, target_peer_id));

                let my_name = st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();
                let target_name = st.online_by_peer.get(&target_peer_id).map(|u| u.name.clone()).unwrap_or_default();

                if let Some(u) = st.online_by_peer.get_mut(&my_id) {
                    u.in_call_with = Some(target_peer_id);
                }
                if let Some(u) = st.online_by_peer.get_mut(&target_peer_id) {
                    u.in_call_with = Some(my_id);
                }

                info!("✅ Call accepted: {} ↔ {} (call_id={})", my_name, target_name, call_id);

                // Уведомляем инициатора
                if let Some(initiator) = st.online_by_peer.get(&target_peer_id) {
                    let _ = initiator.tx.send(ControlMessage::CallAccepted {
                        peer_id: my_id,
                        peer_name: my_name,
                    });
                }
                // Ответившему
                let _ = tx.send(ControlMessage::CallAccepted {
                    peer_id: target_peer_id,
                    peer_name: target_name,
                });
            }

            ControlMessage::CallReject { target_peer_id } => {
                let st = state.lock().await;
                let my_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };
                let my_name = st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();

                if let Some(target) = st.online_by_peer.get(&target_peer_id) {
                    let _ = target.tx.send(ControlMessage::CallRejected {
                        peer_id: my_id,
                        peer_name: my_name,
                    });
                }
            }

            ControlMessage::CallEnd => {
                let mut st = state.lock().await;
                let my_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };

                let partner_id = st.online_by_peer.get(&my_id).and_then(|u| u.in_call_with);
                let my_name = st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();

                if let Some(u) = st.online_by_peer.get_mut(&my_id) {
                    u.in_call_with = None;
                }
                st.active_calls.retain(|_, &mut (a, b)| a != my_id && b != my_id);

                if let Some(pid) = partner_id {
                    if let Some(u) = st.online_by_peer.get_mut(&pid) {
                        u.in_call_with = None;
                    }
                    if let Some(partner) = st.online_by_peer.get(&pid) {
                        let _ = partner.tx.send(ControlMessage::CallEnded {
                            peer_name: my_name,
                        });
                    }
                }
            }

            ControlMessage::Ping => {
                let _ = tx.send(ControlMessage::Pong);
            }

            _ => {}
        }
    }

    if let Some(my_id) = my_peer_id {
        let mut st = state.lock().await;
        let code = my_user_code.unwrap_or_default();
        let partner_id = st.online_by_peer.get(&my_id).and_then(|u| u.in_call_with);
        let my_name = st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();

        if let Some(pid) = partner_id {
            if let Some(u) = st.online_by_peer.get_mut(&pid) {
                u.in_call_with = None;
            }
            if let Some(partner) = st.online_by_peer.get(&pid) {
                let _ = partner.tx.send(ControlMessage::CallEnded {
                    peer_name: my_name,
                });
            }
            st.active_calls.retain(|_, &mut (a, b)| a != my_id && b != my_id);
        }

        st.online_by_peer.remove(&my_id);
        st.online_by_code.remove(&code);
        info!("👋 Offline: peer={}", my_id);
        st.broadcast_status(&code, false, None);
    }

    send_task.abort();
    Ok(())
}

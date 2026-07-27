use anyhow::Result;
use cheburgram_protocol::{
    AudioPacket, ControlMessage, FriendRequestInfo, FriendStatus, TextMessage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::{mpsc, Mutex},
};
use tracing::info;

pub const TCP_SIGNAL_PORT: u16 = 7878;
pub const UDP_MEDIA_PORT: u16 = 7879;
pub const CLIENTS_FILE: &str = "clients.json";

// ─── Реестр клиентов ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientRegistry {
    pub clients: HashMap<String, RegistryEntry>,
    #[serde(default)]
    pub pending_requests: HashMap<String, HashSet<String>>,
    #[serde(default)]
    pub pending_messages: HashMap<String, Vec<TextMessage>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub client_id: String,
    pub name: String,
    pub last_seen: String,
}

impl ClientRegistry {
    pub fn load() -> Self {
        if let Ok(data) = std::fs::read_to_string(CLIENTS_FILE) {
            if let Ok(reg) = serde_json::from_str(&data) {
                return reg;
            }
        }
        Self::default()
    }

    pub fn save(&self) {
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(CLIENTS_FILE, data);
        }
    }

    pub fn generate_code(&self) -> String {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        loop {
            let code = format!("{:06}", rng.gen_range(100_000..999_999));
            if !self.clients.contains_key(&code) {
                return code;
            }
        }
    }

    pub fn upsert(&mut self, user_code: &str, client_id: &str, name: &str) -> String {
        let code = if user_code.len() == 6 && user_code.chars().all(|c| c.is_ascii_digit()) {
            user_code.to_string()
        } else {
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

// ─── Состояние сервера ────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct OnlineUser {
    pub peer_id: u32,
    pub user_code: String,
    pub client_id: String,
    pub name: String,
    pub tx: mpsc::UnboundedSender<ControlMessage>,
    pub udp_addr: Option<SocketAddr>,
    pub in_call_with: Option<u32>,
}

#[derive(Debug, Default)]
pub struct State {
    pub online_by_peer: HashMap<u32, OnlineUser>,
    pub online_by_code: HashMap<String, u32>,
    pub next_peer_id: u32,
    pub next_call_id: u64,
    pub active_calls: HashMap<u64, (u32, u32)>,
}

impl State {
    pub fn broadcast_status(&self, user_code: &str, is_online: bool, peer_id: Option<u32>) {
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

pub type SharedState = Arc<Mutex<State>>;

pub async fn handle_client(
    stream: TcpStream,
    state: SharedState,
    registry: Arc<Mutex<ClientRegistry>>,
) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let (tx, mut rx) = mpsc::unbounded_channel::<ControlMessage>();

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if writer.write_all(format!("{}\n", json).as_bytes()).await.is_err() {
                    break;
                }
                let _ = writer.flush().await;
            }
        }
    });

    let mut my_peer_id: Option<u32> = None;
    let mut my_user_code: Option<String> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        println!("SERVER RECV LINE: {}", line);
        let msg: ControlMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                println!("SERVER PARSE ERR: {:?}", e);
                continue;
            }
        };
        println!("SERVER PARSED MSG: {:?}", msg);

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

                {
                    let reg = registry.lock().await;
                    if let Some(set) = reg.pending_requests.get(&code) {
                        let requests: Vec<FriendRequestInfo> = set.iter().filter_map(|from_code| {
                            reg.clients.get(from_code).map(|entry| FriendRequestInfo {
                                from_code: from_code.clone(),
                                from_name: entry.name.clone(),
                            })
                        }).collect();
                        if !requests.is_empty() {
                            let _ = tx.send(ControlMessage::PendingFriendRequests { requests });
                        }
                    }
                }

                {
                    let mut reg = registry.lock().await;
                    if let Some(pending_msgs) = reg.pending_messages.remove(&code) {
                        if !pending_msgs.is_empty() {
                            let _ = tx.send(ControlMessage::PendingTextMessages {
                                messages: pending_msgs,
                            });
                        }
                        reg.save();
                    }
                }

                st.broadcast_status(&code, true, Some(peer_id));
            }

            ControlMessage::SendFriendRequest { target_code } => {
                let target_clean = target_code.trim().to_string();
                let my_code = match &my_user_code {
                    Some(c) => c.clone(),
                    None => continue,
                };

                let (target_exists, from_name) = {
                    let mut reg = registry.lock().await;
                    let exists = reg.clients.contains_key(&target_clean);
                    let name = reg.clients.get(&my_code).map(|e| e.name.clone()).unwrap_or_default();
                    if exists {
                        reg.pending_requests.entry(target_clean.clone()).or_default().insert(my_code.clone());
                        reg.save();
                    }
                    (exists, name)
                };

                if !target_exists {
                    let _ = tx.send(ControlMessage::Error {
                        message: format!("Пользователь с ID {} не найден", target_clean),
                    });
                } else {
                    let st = state.lock().await;
                    if let Some(&target_peer) = st.online_by_code.get(&target_clean) {
                        if let Some(target_user) = st.online_by_peer.get(&target_peer) {
                            let _ = target_user.tx.send(ControlMessage::IncomingFriendRequest {
                                from_code: my_code.clone(),
                                from_name: from_name.clone(),
                            });
                        }
                    }
                    let _ = tx.send(ControlMessage::Error {
                        message: format!("Запрос в друзья отправлен ID {}!", target_clean),
                    });
                }
            }

            ControlMessage::AcceptFriendRequest { from_code } => {
                let my_code = match &my_user_code {
                    Some(c) => c.clone(),
                    None => continue,
                };

                let (my_name, from_name) = {
                    let mut reg = registry.lock().await;
                    if let Some(set) = reg.pending_requests.get_mut(&my_code) {
                        set.remove(&from_code);
                    }
                    reg.save();
                    let m_name = reg.clients.get(&my_code).map(|e| e.name.clone()).unwrap_or_default();
                    let f_name = reg.clients.get(&from_code).map(|e| e.name.clone()).unwrap_or_default();
                    (m_name, f_name)
                };

                let _ = tx.send(ControlMessage::FriendRequestAccepted {
                    user_code: from_code.clone(),
                    name: from_name,
                });

                let st = state.lock().await;
                if let Some(&sender_peer) = st.online_by_code.get(&from_code) {
                    if let Some(sender_user) = st.online_by_peer.get(&sender_peer) {
                        let _ = sender_user.tx.send(ControlMessage::FriendRequestAccepted {
                            user_code: my_code,
                            name: my_name,
                        });
                    }
                }
            }

            ControlMessage::RejectFriendRequest { from_code } => {
                let my_code = match &my_user_code {
                    Some(c) => c.clone(),
                    None => continue,
                };
                let mut reg = registry.lock().await;
                if let Some(set) = reg.pending_requests.get_mut(&my_code) {
                    set.remove(&from_code);
                }
                reg.save();
            }

            ControlMessage::SendTextMessage { target_code, text, message_id } => {
                let target_clean = target_code.trim().to_string();
                let my_code = match &my_user_code {
                    Some(c) => c.clone(),
                    None => continue,
                };

                let from_name = {
                    let reg = registry.lock().await;
                    reg.clients.get(&my_code).map(|e| e.name.clone()).unwrap_or_default()
                };

                let msg = TextMessage {
                    id: message_id.clone(),
                    from_code: my_code.clone(),
                    from_name: from_name.clone(),
                    to_code: target_clean.clone(),
                    text,
                    timestamp: chrono::Utc::now().to_rfc3339(),
                };

                let st = state.lock().await;
                let target_peer = st.online_by_code.get(&target_clean).copied();

                if let Some(to_id) = target_peer {
                    if let Some(target_user) = st.online_by_peer.get(&to_id) {
                        let _ = target_user.tx.send(ControlMessage::IncomingTextMessage {
                            msg: msg.clone(),
                        });
                        let _ = tx.send(ControlMessage::TextMessageAck {
                            message_id,
                            delivered: true,
                        });
                        info!("💬 SMS delivered: {} -> {}", my_code, target_clean);
                    } else {
                        let mut reg = registry.lock().await;
                        reg.pending_messages.entry(target_clean).or_default().push(msg);
                        reg.save();
                        let _ = tx.send(ControlMessage::TextMessageAck {
                            message_id,
                            delivered: false,
                        });
                    }
                } else {
                    let mut reg = registry.lock().await;
                    reg.pending_messages.entry(target_clean).or_default().push(msg);
                    reg.save();
                    let _ = tx.send(ControlMessage::TextMessageAck {
                        message_id,
                        delivered: false,
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

                let call_id = st.next_call_id + 1;
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

                if let Some(initiator) = st.online_by_peer.get(&target_peer_id) {
                    let _ = initiator.tx.send(ControlMessage::CallAccepted {
                        peer_id: my_id,
                        peer_name: my_name,
                        call_id,
                    });
                }
                let _ = tx.send(ControlMessage::CallAccepted {
                    peer_id: target_peer_id,
                    peer_name: target_name,
                    call_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_generate_code_format() {
        let reg = ClientRegistry::default();
        let code = reg.generate_code();
        assert_eq!(code.len(), 6);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn test_registry_upsert_new_and_existing() {
        let mut reg = ClientRegistry::default();
        let client_id = "test-uuid-1";

        let code1 = reg.upsert("", client_id, "ТестИмя");
        assert_eq!(code1.len(), 6);
        assert_eq!(reg.clients.len(), 1);
        assert_eq!(reg.clients.get(&code1).unwrap().name, "ТестИмя");

        let code2 = reg.upsert("", client_id, "ТестИмя2");
        assert_eq!(code1, code2);
        assert_eq!(reg.clients.get(&code2).unwrap().name, "ТестИмя2");
    }

    #[tokio::test]
    async fn test_state_online_user_tracking() {
        let mut state = State::default();
        let (tx, _rx) = mpsc::unbounded_channel();
        let user = OnlineUser {
            peer_id: 1,
            user_code: "111111".to_string(),
            client_id: "c1".to_string(),
            name: "User1".to_string(),
            tx,
            udp_addr: None,
            in_call_with: None,
        };
        state.online_by_code.insert("111111".to_string(), 1);
        state.online_by_peer.insert(1, user);

        assert_eq!(state.online_by_code.get("111111"), Some(&1));
        assert_eq!(state.online_by_peer.get(&1).unwrap().name, "User1");
    }
}

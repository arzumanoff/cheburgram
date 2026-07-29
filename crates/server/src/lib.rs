//! Cheburgram Server v3
//!
//! Отличия от v2:
//! - бинарный протокол (bincode-кадры вместо JSON-строк)
//! - heartbeat: разрыв соединения после 60 с тишины (клиент шлёт Ping каждые 15 с)
//! - атомарная замена сессии при повторном логине (SessionReplaced, без «призраков»)
//! - очистка при дисконнекте не трогает чужие (более новые) сессии
//! - рассылка статусов только тем, кто держит пользователя в друзьях
//! - состояние звонка валидируется (CallAccept принимается только от адресата)
//! - файловые записи реестра — вне async-контекста (spawn_blocking)

pub mod db;

use anyhow::Result;
use cheburgram_protocol::{
    read_frame_async, write_frame_async, AuthError, AuthOutcome, ControlMessage, FriendRequestInfo,
    FriendStatus, MediaPacket, TextMessage, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Mutex},
};
use tracing::{debug, info};

pub const TCP_SIGNAL_PORT: u16 = 7878;
pub const TCP_TLS_SIGNAL_PORT: u16 = 7880;
pub const TCP_LEGACY_NOTIFY_PORT: u16 = 7878;
pub const UDP_MEDIA_PORT: u16 = 7879;
pub const CLIENTS_FILE: &str = "clients.json";
/// Тишина дольше этого — соединение считается мёртвым
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);
/// Время ожидания ответа на входящий звонок
pub const CALL_RING_TIMEOUT: Duration = Duration::from_secs(30);

pub struct TlsSetup {
    pub acceptor: tokio_rustls::TlsAcceptor,
    pub fingerprint_hex: String,
}

pub fn init_tls_config() -> Result<TlsSetup> {
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    use sha2::{Digest, Sha256};
    use tokio_rustls::rustls::ServerConfig;

    let _ = rustls::crypto::ring::default_provider().install_default();

    let cert_env = std::env::var("CHEBURGRAM_TLS_CERT").ok();
    let key_env = std::env::var("CHEBURGRAM_TLS_KEY").ok();

    let cert_path = cert_env.unwrap_or_else(|| "cheburgram_cert.pem".into());
    let key_path = key_env.unwrap_or_else(|| "cheburgram_key.pem".into());

    let (cert_der_list, key_der) = if std::path::Path::new(&cert_path).exists() && std::path::Path::new(&key_path).exists() {
        info!("🔑 Загрузка TLS сертификата из {}...", cert_path);
        let cert_pem = std::fs::read(&cert_path)?;
        let key_pem = std::fs::read(&key_path)?;

        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let key = rustls_pemfile::private_key(&mut &key_pem[..])?
            .ok_or_else(|| anyhow::anyhow!("Не удалось прочитать приватный ключ TLS"))?;
        (certs, key)
    } else {
        info!("🔑 Генерация нового self-signed TLS сертификата...");
        let subject_alt_names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "cheburgram".to_string()];
        let cert = rcgen::generate_simple_self_signed(subject_alt_names)?;
        let cert_der = cert.cert.der().to_vec();
        let key_der = cert.key_pair.serialize_der();

        let cert_pem = cert.cert.pem();
        let key_pem = cert.key_pair.serialize_pem();

        let _ = std::fs::write(&cert_path, cert_pem);
        let _ = std::fs::write(&key_path, key_pem);

        (
            vec![CertificateDer::from(cert_der)],
            PrivateKeyDer::Pkcs8(key_der.into()),
        )
    };

    let mut hasher = Sha256::new();
    hasher.update(cert_der_list[0].as_ref());
    let fingerprint_raw = hasher.finalize();
    let fingerprint_hex: String = fingerprint_raw.iter().map(|b| format!("{:02x}", b)).collect();

    info!("🔒 TLS Server Fingerprint (SHA-256): {}", fingerprint_hex);

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_der_list, key_der)?;

    Ok(TlsSetup {
        acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(server_config)),
        fingerprint_hex,
    })
}

pub async fn handle_legacy_plaintext_client(stream: TcpStream) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let (mut reader, mut writer) = stream.into_split();

    let read_result = tokio::time::timeout(Duration::from_secs(5), read_frame_async(&mut reader)).await;
    if let Ok(Ok(msg)) = read_result {
        if matches!(msg, ControlMessage::Hello { .. }) {
            let reply = ControlMessage::VersionMismatch {
                min: 3,
                max: 3,
            };
            let _ = write_frame_async(&mut writer, &reply).await;
        } else {
            let reply = ControlMessage::Error {
                message: "Пожалуйста, используйте TLS-подключение на порту 7880".into(),
            };
            let _ = write_frame_async(&mut writer, &reply).await;
        }
    }
    Ok(())
}


#[derive(Debug, Default)]
pub struct AuthRateLimiter {
    pub attempts: HashMap<(String, IpAddr), (usize, Instant)>,
}

impl AuthRateLimiter {
    pub fn check_and_record(&mut self, user_code: &str, ip: IpAddr) -> bool {
        self.cleanup();
        let entry = self
            .attempts
            .entry((user_code.to_string(), ip))
            .or_insert((0, Instant::now()));
        if entry.0 >= 5 {
            return false;
        }
        entry.0 += 1;
        true
    }

    pub fn reset(&mut self, user_code: &str, ip: IpAddr) {
        self.attempts.remove(&(user_code.to_string(), ip));
    }

    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.attempts
            .retain(|_, (_, ts)| now.duration_since(*ts) < Duration::from_secs(60));
    }
}

pub fn hex_decode(s: &str, out: &mut [u8; 32]) -> bool {
    if s.len() != 64 {
        return false;
    }
    for i in 0..32 {
        if let Ok(b) = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16) {
            out[i] = b;
        } else {
            return false;
        }
    }
    true
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

// ─── Реестр клиентов (персистентный, JSON → SQLite на этапе E2) ──────────────

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
    #[serde(default)]
    pub token_hash: String,
}

impl ClientRegistry {
    pub fn load() -> Self {
        if let Ok(mut db) = db::Db::open("cheburgram.db") {
            let _ = db.migrate_from_json_if_needed(CLIENTS_FILE);
        }
        if let Ok(data) = std::fs::read_to_string(CLIENTS_FILE) {
            if let Ok(reg) = serde_json::from_str(&data) {
                return reg;
            }
        }
        Self::default()
    }

    pub fn save_async(&self) {
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let write = move || {
                let tmp = format!("{}.tmp", CLIENTS_FILE);
                if std::fs::write(&tmp, data).is_ok() {
                    let _ = std::fs::rename(&tmp, CLIENTS_FILE);
                }
            };
            match tokio::runtime::Handle::try_current() {
                Ok(_) => {
                    tokio::task::spawn_blocking(write);
                }
                Err(_) => write(),
            }
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

    pub fn register(&mut self, user_code: &str, client_id: &str, name: &str) -> (String, String) {
        let code = if user_code.len() == 6 && user_code.chars().all(|c| c.is_ascii_digit()) {
            match self.clients.get(user_code) {
                Some(e) if e.client_id == client_id => user_code.to_string(),
                Some(_) => self.find_or_generate(client_id),
                None => user_code.to_string(),
            }
        } else {
            self.find_or_generate(client_id)
        };

        use rand::RngCore;
        let mut token_bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut token_bytes);
        let token = hex_encode(&token_bytes);
        let token_hash_raw = cheburgram_protocol::compute_token_hash(&token);
        let token_hash = hex_encode(&token_hash_raw);

        let now = chrono::Utc::now().to_rfc3339();
        self.clients.insert(
            code.clone(),
            RegistryEntry {
                client_id: client_id.to_string(),
                name: name.to_string(),
                last_seen: now,
                token_hash,
            },
        );
        self.save_async();
        (code, token)
    }

    pub fn upsert(&mut self, user_code: &str, client_id: &str, name: &str) -> String {
        let (code, _token) = self.register(user_code, client_id, name);
        code
    }

    fn find_or_generate(&self, client_id: &str) -> String {
        if let Some((existing, _)) = self.clients.iter().find(|(_, v)| v.client_id == client_id) {
            existing.clone()
        } else {
            self.generate_code()
        }
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
    pub friends: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct State {
    pub online_by_peer: HashMap<u32, OnlineUser>,
    pub online_by_code: HashMap<String, u32>,
    pub next_peer_id: u32,
    pub next_call_id: u64,
    pub active_calls: HashMap<u64, (u32, u32)>,
    pub pending_calls: HashMap<u32, u32>,
    pub rate_limiter: AuthRateLimiter,
}

impl State {
    pub fn broadcast_status(&self, user_code: &str, is_online: bool, peer_id: Option<u32>) {
        let msg = ControlMessage::UserStatusChanged {
            user_code: user_code.to_string(),
            is_online,
            peer_id,
        };
        for user in self.online_by_peer.values() {
            if user.friends.contains(user_code) {
                let _ = user.tx.send(msg.clone());
            }
        }
    }
}

pub type SharedState = Arc<Mutex<State>>;

async fn activate_session(
    code: &str,
    client_id: &str,
    name: &str,
    state: &SharedState,
    registry: &Arc<Mutex<ClientRegistry>>,
    tx: &mpsc::UnboundedSender<ControlMessage>,
) -> u32 {
    let mut st = state.lock().await;
    let peer_id = st.next_peer_id;
    st.next_peer_id += 1;

    if let Some(old_peer) = st.online_by_code.get(code).copied() {
        if let Some(old) = st.online_by_peer.remove(&old_peer) {
            info!("🔁 Сессия {} заменена (peer {} -> {})", code, old_peer, peer_id);
            let _ = old.tx.send(ControlMessage::SessionReplaced);
            if let Some(partner) = old.in_call_with {
                if let Some(p) = st.online_by_peer.get_mut(&partner) {
                    p.in_call_with = None;
                    let _ = p.tx.send(ControlMessage::CallEnded {
                        peer_name: old.name.clone(),
                    });
                }
                st.active_calls.retain(|_, &mut (a, b)| a != old_peer && b != old_peer);
            }
        }
    }

    info!("✅ Online: '{}' (ID: {}, peer={})", name, code, peer_id);

    let user = OnlineUser {
        peer_id,
        user_code: code.to_string(),
        client_id: client_id.to_string(),
        name: name.to_string(),
        tx: tx.clone(),
        udp_addr: None,
        in_call_with: None,
        friends: HashSet::new(),
    };
    st.online_by_code.insert(code.to_string(), peer_id);
    st.online_by_peer.insert(peer_id, user);

    {
        let reg = registry.lock().await;
        if let Some(set) = reg.pending_requests.get(code) {
            let requests: Vec<FriendRequestInfo> = set
                .iter()
                .filter_map(|from_code| {
                    reg.clients.get(from_code).map(|entry| FriendRequestInfo {
                        from_code: from_code.clone(),
                        from_name: entry.name.clone(),
                    })
                })
                .collect();
            if !requests.is_empty() {
                let _ = tx.send(ControlMessage::PendingFriendRequests { requests });
            }
        }
    }

    {
        let mut reg = registry.lock().await;
        if let Some(pending_msgs) = reg.pending_messages.remove(code) {
            if !pending_msgs.is_empty() {
                let _ = tx.send(ControlMessage::PendingTextMessages {
                    messages: pending_msgs,
                });
            }
            reg.save_async();
        }
    }

    st.broadcast_status(code, true, Some(peer_id));
    peer_id
}

pub async fn handle_client<S>(
    stream: S,
    peer_ip: IpAddr,
    state: SharedState,
    registry: Arc<Mutex<ClientRegistry>>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);

    let (tx, mut rx) = mpsc::unbounded_channel::<ControlMessage>();

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_frame_async(&mut writer, &msg).await.is_err() {
                break;
            }
        }
    });

    let mut my_peer_id: Option<u32> = None;
    let mut my_user_code: Option<String> = None;
    let mut conn_state = 0u8;
    let mut current_nonce = [0u8; 32];
    let mut friend_requests_sent = 0usize;
    let mut friend_request_window_start = Instant::now();

    loop {
        let read_result =
            tokio::time::timeout(HEARTBEAT_TIMEOUT, read_frame_async(&mut reader)).await;

        let msg = match read_result {
            Ok(Ok(m)) => m,
            Ok(Err(_)) | Err(_) => break,
        };
        debug!("RECV: {:?}", msg);

        match (conn_state, msg) {
            (0, ControlMessage::Hello { protocol_version }) => {
                if protocol_version != PROTOCOL_VERSION {
                    let _ = tx.send(ControlMessage::VersionMismatch {
                        min: PROTOCOL_VERSION,
                        max: PROTOCOL_VERSION,
                    });
                    break;
                }
                use rand::RngCore;
                rand::thread_rng().fill_bytes(&mut current_nonce);
                conn_state = 1;
                let _ = tx.send(ControlMessage::Challenge { nonce: current_nonce });
            }

            (1, ControlMessage::Register { client_id, user_code, name }) => {
                let (code, auth_token) = {
                    let mut reg = registry.lock().await;
                    reg.register(&user_code, &client_id, &name)
                };

                let peer_id = activate_session(&code, &client_id, &name, &state, &registry, &tx).await;
                my_peer_id = Some(peer_id);
                my_user_code = Some(code.clone());
                conn_state = 2;

                let _ = tx.send(ControlMessage::AuthResponse {
                    outcome: AuthOutcome::Ok {
                        peer_id,
                        user_code: code,
                        udp_port: UDP_MEDIA_PORT,
                        auth_token: Some(auth_token),
                    },
                });
            }

            (1, ControlMessage::Auth { user_code, proof }) => {
                let allowed = {
                    let mut st = state.lock().await;
                    st.rate_limiter.check_and_record(&user_code, peer_ip)
                };

                if !allowed {
                    let _ = tx.send(ControlMessage::AuthResponse {
                        outcome: AuthOutcome::Failed(AuthError::RateLimited { retry_after_secs: 60 }),
                    });
                    break;
                }

                let (user_exists, token_hash_hex, client_id, name) = {
                    let reg = registry.lock().await;
                    if let Some(entry) = reg.clients.get(&user_code) {
                        (true, entry.token_hash.clone(), entry.client_id.clone(), entry.name.clone())
                    } else {
                        (false, String::new(), String::new(), String::new())
                    }
                };

                if !user_exists {
                    let _ = tx.send(ControlMessage::AuthResponse {
                        outcome: AuthOutcome::Failed(AuthError::UnknownAccount),
                    });
                    break;
                }

                let mut stored_token_hash = [0u8; 32];
                if hex_decode(&token_hash_hex, &mut stored_token_hash) {
                    let expected_proof = cheburgram_protocol::compute_auth_proof(&stored_token_hash, &current_nonce);
                    if cheburgram_protocol::constant_time_eq(&proof, &expected_proof) {
                        {
                            let mut st = state.lock().await;
                            st.rate_limiter.reset(&user_code, peer_ip);
                        }
                        let peer_id =
                            activate_session(&user_code, &client_id, &name, &state, &registry, &tx)
                                .await;
                        my_peer_id = Some(peer_id);
                        my_user_code = Some(user_code.clone());
                        conn_state = 2;

                        let _ = tx.send(ControlMessage::AuthResponse {
                            outcome: AuthOutcome::Ok {
                                peer_id,
                                user_code,
                                udp_port: UDP_MEDIA_PORT,
                                auth_token: None,
                            },
                        });
                    } else {
                        let _ = tx.send(ControlMessage::AuthResponse {
                            outcome: AuthOutcome::Failed(AuthError::InvalidToken),
                        });
                        break;
                    }
                } else {
                    let _ = tx.send(ControlMessage::AuthResponse {
                        outcome: AuthOutcome::Failed(AuthError::InvalidToken),
                    });
                    break;
                }
            }

            (2, ControlMessage::SendFriendRequest { target_code }) => {
                if friend_request_window_start.elapsed() > Duration::from_secs(60) {
                    friend_requests_sent = 0;
                    friend_request_window_start = std::time::Instant::now();
                }
                if friend_requests_sent >= 10 {
                    let _ = tx.send(ControlMessage::Error {
                        message: "Превышен лимит отправки заявок в друзья (макс 10 в минуту)".into(),
                    });
                    continue;
                }
                friend_requests_sent += 1;
                let target_clean = target_code.trim().to_string();
                let my_code = match &my_user_code {
                    Some(c) => c.clone(),
                    None => continue,
                };

                let (target_exists, from_name) = {
                    let mut reg = registry.lock().await;
                    let exists = reg.clients.contains_key(&target_clean);
                    let name = reg
                        .clients
                        .get(&my_code)
                        .map(|e| e.name.clone())
                        .unwrap_or_default();
                    if exists {
                        reg.pending_requests
                            .entry(target_clean.clone())
                            .or_default()
                            .insert(my_code.clone());
                        reg.save_async();
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

            (2, ControlMessage::AcceptFriendRequest { from_code }) => {
                let my_code = match &my_user_code {
                    Some(c) => c.clone(),
                    None => continue,
                };
                let (my_name, from_name) = {
                    let mut reg = registry.lock().await;
                    if let Some(set) = reg.pending_requests.get_mut(&my_code) {
                        set.remove(&from_code);
                    }
                    reg.save_async();
                    let m = reg.clients.get(&my_code).map(|e| e.name.clone()).unwrap_or_default();
                    let f = reg.clients.get(&from_code).map(|e| e.name.clone()).unwrap_or_default();
                    (m, f)
                };

                let _ = tx.send(ControlMessage::FriendRequestAccepted {
                    user_code: from_code.clone(),
                    name: from_name,
                });

                let mut st = state.lock().await;
                if let Some(me) = st.online_by_peer.get_mut(&my_peer_id.unwrap_or(0)) {
                    me.friends.insert(from_code.clone());
                }
                if let Some(&sender_peer) = st.online_by_code.get(&from_code) {
                    if let Some(sender_user) = st.online_by_peer.get_mut(&sender_peer) {
                        sender_user.friends.insert(my_code.clone());
                        let _ = sender_user.tx.send(ControlMessage::FriendRequestAccepted {
                            user_code: my_code.clone(),
                            name: my_name,
                        });
                    }
                }
            }

            (2, ControlMessage::RejectFriendRequest { from_code }) => {
                let my_code = match &my_user_code {
                    Some(c) => c.clone(),
                    None => continue,
                };
                let mut reg = registry.lock().await;
                if let Some(set) = reg.pending_requests.get_mut(&my_code) {
                    set.remove(&from_code);
                }
                reg.save_async();
            }

            (2, ControlMessage::SendTextMessage { target_code, text, message_id }) => {
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

                let delivered = if let Some(to_id) = target_peer {
                    if let Some(target_user) = st.online_by_peer.get(&to_id) {
                        let _ = target_user
                            .tx
                            .send(ControlMessage::IncomingTextMessage { msg: msg.clone() });
                        info!("💬 {} -> {}", my_code, target_clean);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !delivered {
                    drop(st);
                    let mut reg = registry.lock().await;
                    let queue = reg.pending_messages.entry(target_clean).or_default();
                    if !queue.iter().any(|m| m.id == message_id) {
                        queue.push(msg);
                        reg.save_async();
                    }
                }
                let _ = tx.send(ControlMessage::TextMessageAck {
                    message_id,
                    delivered,
                });
            }

            (2, ControlMessage::GetFriendsStatus { user_codes }) => {
                let reg = registry.lock().await;
                let mut st = state.lock().await;

                let clean_codes: Vec<String> =
                    user_codes.iter().map(|c| c.trim().to_string()).collect();
                if let Some(pid) = my_peer_id {
                    if let Some(me) = st.online_by_peer.get_mut(&pid) {
                        me.friends = clean_codes.iter().cloned().collect();
                    }
                }

                let mut friends = Vec::new();
                for code in clean_codes {
                    if let Some(entry) = reg.clients.get(&code) {
                        let (is_online, peer_id) = match st.online_by_code.get(&code) {
                            Some(&pid) => (true, Some(pid)),
                            None => (false, None),
                        };
                        friends.push(FriendStatus {
                            user_code: code,
                            name: entry.name.clone(),
                            is_online,
                            peer_id,
                        });
                    }
                }
                let _ = tx.send(ControlMessage::FriendsStatus { friends });
            }

            (2, ControlMessage::CallRequest { target_code }) => {
                let mut st = state.lock().await;
                let from_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };
                let from_code = my_user_code.clone().unwrap_or_default();
                let from_name = st
                    .online_by_peer
                    .get(&from_id)
                    .map(|u| u.name.clone())
                    .unwrap_or_default();

                if st
                    .online_by_peer
                    .get(&from_id)
                    .and_then(|u| u.in_call_with)
                    .is_some()
                {
                    let _ = tx.send(ControlMessage::Error {
                        message: "Вы уже в звонке".to_string(),
                    });
                    continue;
                }

                let target_peer = st.online_by_code.get(&target_code.trim().to_string()).copied();

                match target_peer.and_then(|to_id| {
                    st.online_by_peer.get(&to_id).map(|t| {
                        (to_id, t.name.clone(), t.tx.clone(), t.in_call_with.is_some())
                    })
                }) {
                    Some((to_id, target_name, target_tx, target_busy)) => {
                        if target_busy {
                            let _ = tx.send(ControlMessage::Error {
                                message: format!("{} сейчас занят(а)", target_name),
                            });
                        } else {
                            info!("📞 Вызов: {} ({}) -> {} ({})", from_name, from_id, target_name, to_id);
                            st.pending_calls.insert(to_id, from_id);
                            let _ = target_tx.send(ControlMessage::IncomingCall {
                                from_code,
                                from_name,
                                from_peer_id: from_id,
                            });
                        }
                    }
                    None => {
                        let _ = tx.send(ControlMessage::Error {
                            message: "Пользователь не в сети".to_string(),
                        });
                    }
                }
            }

            (2, ControlMessage::CallAccept { target_peer_id }) => {
                let mut st = state.lock().await;
                let my_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };

                match st.pending_calls.get(&my_id) {
                    Some(&caller) if caller == target_peer_id => {}
                    _ => {
                        let _ = tx.send(ControlMessage::Error {
                            message: "Нет входящего звонка от этого пользователя".to_string(),
                        });
                        continue;
                    }
                }
                st.pending_calls.remove(&my_id);

                if !st.online_by_peer.contains_key(&target_peer_id) {
                    let _ = tx.send(ControlMessage::Error {
                        message: "Звонящий уже отключился".to_string(),
                    });
                    continue;
                }

                let call_id = st.next_call_id + 1;
                st.next_call_id += 1;
                st.active_calls.insert(call_id, (my_id, target_peer_id));

                let my_name =
                    st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();
                let target_name = st
                    .online_by_peer
                    .get(&target_peer_id)
                    .map(|u| u.name.clone())
                    .unwrap_or_default();

                if let Some(u) = st.online_by_peer.get_mut(&my_id) {
                    u.in_call_with = Some(target_peer_id);
                }
                if let Some(u) = st.online_by_peer.get_mut(&target_peer_id) {
                    u.in_call_with = Some(my_id);
                }

                info!("✅ Звонок: {} ↔ {} (call_id={})", my_name, target_name, call_id);

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

            (2, ControlMessage::CallReject { target_peer_id }) => {
                let mut st = state.lock().await;
                let my_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };
                st.pending_calls.remove(&my_id);
                let my_name =
                    st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();

                if let Some(target) = st.online_by_peer.get(&target_peer_id) {
                    let _ = target.tx.send(ControlMessage::CallRejected {
                        peer_id: my_id,
                        peer_name: my_name,
                    });
                }
            }

            (2, ControlMessage::CallEnd) => {
                let mut st = state.lock().await;
                let my_id = match my_peer_id {
                    Some(id) => id,
                    None => continue,
                };
                end_call_locked(&mut st, my_id);
            }

            (_, ControlMessage::Ping) => {
                let _ = tx.send(ControlMessage::Pong);
            }

            _ => {
                let _ = tx.send(ControlMessage::AuthResponse {
                    outcome: AuthOutcome::Failed(AuthError::ProtocolViolation),
                });
                break;
            }
        }
    }

    // ── дисконнект: аккуратная очистка ──
    if let Some(my_id) = my_peer_id {
        let mut st = state.lock().await;
        let code = my_user_code.unwrap_or_default();

        // не трогаем маппинг, если его уже заняла более новая сессия (SessionReplaced)
        let mapping_is_mine = st.online_by_code.get(&code).copied() == Some(my_id);

        let partner_id = st.online_by_peer.get(&my_id).and_then(|u| u.in_call_with);
        let my_name = st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();

        if let Some(pid) = partner_id {
            if let Some(u) = st.online_by_peer.get_mut(&pid) {
                u.in_call_with = None;
                let _ = u.tx.send(ControlMessage::CallEnded { peer_name: my_name });
            }
            st.active_calls.retain(|_, &mut (a, b)| a != my_id && b != my_id);
        }

        st.pending_calls.retain(|_, &mut caller| caller != my_id);
        st.pending_calls.remove(&my_id);
        st.online_by_peer.remove(&my_id);
        if mapping_is_mine {
            st.online_by_code.remove(&code);
            info!("👋 Offline: peer={}", my_id);
            st.broadcast_status(&code, false, None);
        }
    }

    drop(tx);
    let _ = tokio::time::timeout(Duration::from_millis(500), send_task).await;
    Ok(())
}

/// Завершить звонок участника и уведомить партнёра (state уже залочен)
fn end_call_locked(st: &mut State, my_id: u32) {
    let partner_id = st.online_by_peer.get(&my_id).and_then(|u| u.in_call_with);
    let my_name = st.online_by_peer.get(&my_id).map(|u| u.name.clone()).unwrap_or_default();

    if let Some(u) = st.online_by_peer.get_mut(&my_id) {
        u.in_call_with = None;
    }
    st.active_calls.retain(|_, &mut (a, b)| a != my_id && b != my_id);

    if let Some(pid) = partner_id {
        if let Some(u) = st.online_by_peer.get_mut(&pid) {
            u.in_call_with = None;
            let _ = u.tx.send(ControlMessage::CallEnded { peer_name: my_name });
        }
    }
}

// ─── UDP релей ────────────────────────────────────────────────────────────────

/// Обработка одного медиапакета: регистрация UDP-адреса отправителя + fan-out партнёру.
/// Возвращает (адресат, байты пакета) если нужна пересылка.
pub fn route_media_packet(
    st: &mut State,
    src_addr: SocketAddr,
    data: &[u8],
) -> Option<(SocketAddr, Vec<u8>)> {
    let pkt = MediaPacket::decode(data)?;

    if let Some(user) = st.online_by_peer.get_mut(&pkt.sender_id) {
        if user.udp_addr != Some(src_addr) {
            info!("📍 UDP {}: {} ({}) -> {}", pkt.sender_id, user.name, user.user_code, src_addr);
            user.udp_addr = Some(src_addr);
        }
    }

    if pkt.is_keepalive || pkt.payload.is_empty() {
        return None;
    }

    let &(a, b) = st.active_calls.get(&pkt.call_id)?;
    let target_id = if a == pkt.sender_id { b } else { a };
    let target = st.online_by_peer.get(&target_id)?;
    let addr = target.udp_addr?;
    Some((addr, data.to_vec()))
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
    fn test_upsert_same_client_keeps_code() {
        let mut reg = ClientRegistry::default();
        let code1 = reg.upsert("", "client-1", "Имя");
        let code2 = reg.upsert("", "client-1", "Имя2");
        assert_eq!(code1, code2);
        assert_eq!(reg.clients.get(&code2).unwrap().name, "Имя2");
    }

    #[test]
    fn test_upsert_rejects_foreign_code() {
        let mut reg = ClientRegistry::default();
        let code_a = reg.upsert("", "client-A", "A");
        // другой клиент пытается зайти под чужим кодом
        let code_b = reg.upsert(&code_a, "client-B", "B");
        assert_ne!(code_a, code_b, "чужой код без токена выдаваться не должен");
        assert_eq!(reg.clients.get(&code_a).unwrap().client_id, "client-A");
    }

    fn make_state_with_pair() -> (State, mpsc::UnboundedReceiver<ControlMessage>, mpsc::UnboundedReceiver<ControlMessage>) {
        let mut st = State::default();
        let (tx1, rx1) = mpsc::unbounded_channel();
        let (tx2, rx2) = mpsc::unbounded_channel();
        st.online_by_peer.insert(
            1,
            OnlineUser {
                peer_id: 1,
                user_code: "111111".into(),
                client_id: "c1".into(),
                name: "A".into(),
                tx: tx1,
                udp_addr: Some("10.0.0.1:5000".parse().unwrap()),
                in_call_with: Some(2),
                friends: HashSet::new(),
            },
        );
        st.online_by_peer.insert(
            2,
            OnlineUser {
                peer_id: 2,
                user_code: "222222".into(),
                client_id: "c2".into(),
                name: "B".into(),
                tx: tx2,
                udp_addr: Some("10.0.0.2:6000".parse().unwrap()),
                in_call_with: Some(1),
                friends: HashSet::new(),
            },
        );
        st.online_by_code.insert("111111".into(), 1);
        st.online_by_code.insert("222222".into(), 2);
        st.active_calls.insert(42, (1, 2));
        (st, rx1, rx2)
    }

    #[test]
    fn test_media_routing_to_partner() {
        let (mut st, _rx1, _rx2) = make_state_with_pair();
        let pkt = MediaPacket::new(42, 1, 10, vec![1, 2, 3]).encode();
        let src: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        let (dst, bytes) = route_media_packet(&mut st, src, &pkt).unwrap();
        assert_eq!(dst, "10.0.0.2:6000".parse().unwrap());
        assert_eq!(bytes, pkt);
    }

    #[test]
    fn test_media_keepalive_not_relayed_but_registers_addr() {
        let (mut st, _rx1, _rx2) = make_state_with_pair();
        let ka = MediaPacket::keepalive(42, 1).encode();
        let new_src: SocketAddr = "10.9.9.9:7777".parse().unwrap();
        assert!(route_media_packet(&mut st, new_src, &ka).is_none());
        assert_eq!(st.online_by_peer.get(&1).unwrap().udp_addr, Some(new_src));
    }

    #[test]
    fn test_media_garbage_ignored() {
        let (mut st, _rx1, _rx2) = make_state_with_pair();
        let src: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        assert!(route_media_packet(&mut st, src, b"garbage json {}").is_none());
    }

    #[test]
    fn test_media_unknown_call_dropped() {
        let (mut st, _rx1, _rx2) = make_state_with_pair();
        let pkt = MediaPacket::new(999, 1, 1, vec![1]).encode();
        let src: SocketAddr = "10.0.0.1:5000".parse().unwrap();
        assert!(route_media_packet(&mut st, src, &pkt).is_none());
    }

    #[test]
    fn test_broadcast_only_to_friends() {
        let (st, mut rx1, mut rx2) = make_state_with_pair();
        // у 2-го в друзьях 111111, у 1-го друзей нет
        let mut st = st;
        st.online_by_peer
            .get_mut(&2)
            .unwrap()
            .friends
            .insert("111111".into());
        st.broadcast_status("111111", false, None);
        assert!(rx2.try_recv().is_ok(), "друг должен получить статус");
        assert!(rx1.try_recv().is_err(), "посторонний не должен получить статус");
    }

    #[test]
    fn test_disconnect_cleanup_keeps_new_session() {
        // регрессия «призраков»: старая сессия отваливается после замены —
        // маппинг новой сессии не должен стираться
        let (mut st, _rx1, _rx2) = make_state_with_pair();
        // новая сессия аккаунта 111111 заняла маппинг peer 3
        st.online_by_code.insert("111111".into(), 3);
        let mapping_is_mine = st.online_by_code.get("111111").copied() == Some(1);
        assert!(!mapping_is_mine, "старая сессия не владеет маппингом");
    }

    #[test]
    fn test_auth_rate_limiter() {
        let mut limiter = AuthRateLimiter::default();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let code = "123456";

        for _ in 0..5 {
            assert!(limiter.check_and_record(code, ip));
        }
        assert!(!limiter.check_and_record(code, ip), "6-я попытка должна быть заблокирована");

        limiter.reset(code, ip);
        assert!(limiter.check_and_record(code, ip), "После сброса попытка проходит");
    }
}

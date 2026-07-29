//! Cheburgram Protocol v2
//!
//! Отличия от v1 (JSON):
//! - TCP-сигналинг: бинарные кадры [u32 LE длина][bincode payload]
//! - UDP-медиа: фиксированный заголовок 20 байт + Opus payload
//! - Heartbeat Ping/Pong и SessionReplaced для корректной замены сессий

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 3;
pub const MEDIA_MAGIC: u16 = 0x4347; // "CG"
pub const MEDIA_VERSION: u8 = 3;
pub const MAX_FRAME_SIZE: u32 = 1 << 20; // 1 МБ — защита от мусора в потоке

// ─── Данные ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FriendStatus {
    pub user_code: String,
    pub name: String,
    pub is_online: bool,
    pub peer_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FriendRequestInfo {
    pub from_code: String,
    pub from_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextMessage {
    pub id: String,
    pub from_code: String,
    pub from_name: String,
    pub to_code: String,
    pub text: String,
    pub timestamp: String, // ISO 8601
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallRecord {
    pub peer_name: String,
    /// ID собеседника (для быстрых действий из истории; пусто у старых записей)
    #[serde(default)]
    pub peer_code: String,
    pub direction: CallDirection,
    pub timestamp: String,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CallDirection {
    Incoming,
    Outgoing,
    Missed,
}

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

pub type HmacSha256 = Hmac<Sha256>;

pub fn compute_token_hash(token: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

pub fn compute_auth_proof(token_hash: &[u8; 32], nonce: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(token_hash).expect("HMAC supports 32-byte key");
    mac.update(nonce);
    mac.finalize().into_bytes().into()
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut res = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        res |= x ^ y;
    }
    res == 0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AuthError {
    InvalidToken,
    UnknownAccount,
    RateLimited { retry_after_secs: u32 },
    ProtocolViolation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AuthOutcome {
    Ok {
        peer_id: u32,
        user_code: String,
        udp_port: u16,
        auth_token: Option<String>,
    },
    Failed(AuthError),
}

// ─── Сигнальные сообщения (TCP) ─────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum ControlMessage {
    // === Рукопожатие и регистрация ===
    Hello {
        protocol_version: u16,
    },
    Challenge {
        nonce: [u8; 32],
    },
    VersionMismatch {
        min: u16,
        max: u16,
    },
    Register {
        client_id: String,
        user_code: String,
        name: String,
    },
    Registered {
        peer_id: u32,
        user_code: String,
        auth_token: Option<String>,
        udp_port: u16,
    },
    Auth {
        user_code: String,
        proof: [u8; 32],
    },
    AuthResponse {
        outcome: AuthOutcome,
    },
    /// Сервер разрывает старую сессию этого же аккаунта
    SessionReplaced,

    // === Друзья ===
    SendFriendRequest {
        target_code: String,
    },
    IncomingFriendRequest {
        from_code: String,
        from_name: String,
    },
    AcceptFriendRequest {
        from_code: String,
    },
    RejectFriendRequest {
        from_code: String,
    },
    FriendRequestAccepted {
        user_code: String,
        name: String,
    },
    GetFriendsStatus {
        user_codes: Vec<String>,
    },
    FriendsStatus {
        friends: Vec<FriendStatus>,
    },
    PendingFriendRequests {
        requests: Vec<FriendRequestInfo>,
    },

    // === Чат ===
    SendTextMessage {
        target_code: String,
        text: String,
        message_id: String,
    },
    IncomingTextMessage {
        msg: TextMessage,
    },
    TextMessageAck {
        message_id: String,
        delivered: bool,
    },
    PendingTextMessages {
        messages: Vec<TextMessage>,
    },

    // === Присутствие ===
    UserStatusChanged {
        user_code: String,
        is_online: bool,
        peer_id: Option<u32>,
    },

    // === Звонки ===
    CallRequest {
        target_code: String,
    },
    IncomingCall {
        from_code: String,
        from_name: String,
        from_peer_id: u32,
    },
    CallAccept {
        target_peer_id: u32,
    },
    CallAccepted {
        peer_id: u32,
        peer_name: String,
        call_id: u64,
    },
    CallReject {
        target_peer_id: u32,
    },
    CallRejected {
        peer_id: u32,
        peer_name: String,
    },
    CallEnd,
    CallEnded {
        peer_name: String,
    },
    CallMissed {
        peer_name: String,
    },

    // === Служебные ===
    Ping,
    Pong,
    Error {
        message: String,
    },
}

impl std::fmt::Debug for ControlMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlMessage::Hello { protocol_version } => f.debug_struct("Hello").field("protocol_version", protocol_version).finish(),
            ControlMessage::Challenge { nonce: _ } => f.debug_struct("Challenge").field("nonce", &"<32 bytes>").finish(),
            ControlMessage::VersionMismatch { min, max } => f.debug_struct("VersionMismatch").field("min", min).field("max", max).finish(),
            ControlMessage::Register { client_id, user_code, name } => f.debug_struct("Register").field("client_id", client_id).field("user_code", user_code).field("name", name).finish(),
            ControlMessage::Registered { peer_id, user_code, auth_token, udp_port } => {
                let redacted = auth_token.as_ref().map(|_| "<redacted>");
                f.debug_struct("Registered").field("peer_id", peer_id).field("user_code", user_code).field("auth_token", &redacted).field("udp_port", udp_port).finish()
            }
            ControlMessage::Auth { user_code, proof: _ } => f.debug_struct("Auth").field("user_code", user_code).field("proof", &"<redacted>").finish(),
            ControlMessage::AuthResponse { outcome } => {
                match outcome {
                    AuthOutcome::Ok { peer_id, user_code, udp_port, auth_token } => {
                        let redacted = auth_token.as_ref().map(|_| "<redacted>");
                        f.debug_struct("AuthResponse::Ok").field("peer_id", peer_id).field("user_code", user_code).field("auth_token", &redacted).field("udp_port", udp_port).finish()
                    }
                    AuthOutcome::Failed(err) => f.debug_struct("AuthResponse::Failed").field("error", err).finish(),
                }
            }
            ControlMessage::SessionReplaced => f.write_str("SessionReplaced"),
            ControlMessage::SendFriendRequest { target_code } => f.debug_struct("SendFriendRequest").field("target_code", target_code).finish(),
            ControlMessage::IncomingFriendRequest { from_code, from_name } => f.debug_struct("IncomingFriendRequest").field("from_code", from_code).field("from_name", from_name).finish(),
            ControlMessage::AcceptFriendRequest { from_code } => f.debug_struct("AcceptFriendRequest").field("from_code", from_code).finish(),
            ControlMessage::RejectFriendRequest { from_code } => f.debug_struct("RejectFriendRequest").field("from_code", from_code).finish(),
            ControlMessage::FriendRequestAccepted { user_code, name } => f.debug_struct("FriendRequestAccepted").field("user_code", user_code).field("name", name).finish(),
            ControlMessage::GetFriendsStatus { user_codes } => f.debug_struct("GetFriendsStatus").field("user_codes", user_codes).finish(),
            ControlMessage::FriendsStatus { friends } => f.debug_struct("FriendsStatus").field("friends", friends).finish(),
            ControlMessage::PendingFriendRequests { requests } => f.debug_struct("PendingFriendRequests").field("requests", requests).finish(),
            ControlMessage::SendTextMessage { target_code, text, message_id } => f.debug_struct("SendTextMessage").field("target_code", target_code).field("text", text).field("message_id", message_id).finish(),
            ControlMessage::IncomingTextMessage { msg } => f.debug_struct("IncomingTextMessage").field("msg", msg).finish(),
            ControlMessage::TextMessageAck { message_id, delivered } => f.debug_struct("TextMessageAck").field("message_id", message_id).field("delivered", delivered).finish(),
            ControlMessage::PendingTextMessages { messages } => f.debug_struct("PendingTextMessages").field("messages", messages).finish(),
            ControlMessage::UserStatusChanged { user_code, is_online, peer_id } => f.debug_struct("UserStatusChanged").field("user_code", user_code).field("is_online", is_online).field("peer_id", peer_id).finish(),
            ControlMessage::CallRequest { target_code } => f.debug_struct("CallRequest").field("target_code", target_code).finish(),
            ControlMessage::IncomingCall { from_code, from_name, from_peer_id } => f.debug_struct("IncomingCall").field("from_code", from_code).field("from_name", from_name).field("from_peer_id", from_peer_id).finish(),
            ControlMessage::CallAccept { target_peer_id } => f.debug_struct("CallAccept").field("target_peer_id", target_peer_id).finish(),
            ControlMessage::CallAccepted { peer_id, peer_name, call_id } => f.debug_struct("CallAccepted").field("peer_id", peer_id).field("peer_name", peer_name).field("call_id", call_id).finish(),
            ControlMessage::CallReject { target_peer_id } => f.debug_struct("CallReject").field("target_peer_id", target_peer_id).finish(),
            ControlMessage::CallRejected { peer_id, peer_name } => f.debug_struct("CallRejected").field("peer_id", peer_id).field("peer_name", peer_name).finish(),
            ControlMessage::CallEnd => f.write_str("CallEnd"),
            ControlMessage::CallEnded { peer_name } => f.debug_struct("CallEnded").field("peer_name", peer_name).finish(),
            ControlMessage::CallMissed { peer_name } => f.debug_struct("CallMissed").field("peer_name", peer_name).finish(),
            ControlMessage::Ping => f.write_str("Ping"),
            ControlMessage::Pong => f.write_str("Pong"),
            ControlMessage::Error { message } => f.debug_struct("Error").field("message", message).finish(),
        }
    }
}

// ─── Бинарная сериализация TCP-кадров ───────────────────────────────────────

pub fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// Сериализовать сообщение в кадр [u32 LE len][payload]
pub fn encode_frame(msg: &ControlMessage) -> Result<Vec<u8>, bincode::error::EncodeError> {
    let payload = bincode::serde::encode_to_vec(msg, bincode_config())?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Десериализовать payload кадра (без префикса длины)
pub fn decode_payload(payload: &[u8]) -> Result<ControlMessage, bincode::error::DecodeError> {
    let (msg, _read) = bincode::serde::decode_from_slice(payload, bincode_config())?;
    Ok(msg)
}

/// Синхронное чтение кадра (клиент, std::io)
pub fn read_frame_sync(reader: &mut impl std::io::Read) -> std::io::Result<ControlMessage> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf);
    if len == 0 || len > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("недопустимый размер кадра: {}", len),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf)?;
    decode_payload(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Синхронная запись кадра (клиент, std::io)
pub fn write_frame_sync(
    writer: &mut impl std::io::Write,
    msg: &ControlMessage,
) -> std::io::Result<()> {
    let frame = encode_frame(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    writer.write_all(&frame)?;
    writer.flush()
}

/// Асинхронное чтение кадра (сервер, tokio)
pub async fn read_frame_async<R>(reader: &mut R) -> std::io::Result<ControlMessage>
where
    R: tokio::io::AsyncReadExt + Unpin,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);
    if len == 0 || len > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("недопустимый размер кадра: {}", len),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    decode_payload(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Асинхронная запись кадра (сервер, tokio)
pub async fn write_frame_async<W>(
    writer: &mut W,
    msg: &ControlMessage,
) -> std::io::Result<()>
where
    W: tokio::io::AsyncWriteExt + Unpin,
{
    let frame = encode_frame(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    writer.write_all(&frame).await?;
    writer.flush().await
}

// ─── UDP медиа-пакет (заголовок 20 байт) ────────────────────────────────────
//
// [ magic u16 | version u8 | flags u8 | call_id u64 | sender_id u32 | seq u32 | payload... ]
// flags: bit0 = keepalive (payload пустой, только регистрация адреса на релее)

pub const FLAG_KEEPALIVE: u8 = 0x01;
pub const MEDIA_WIRE_HEADER_LEN: usize = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct MediaPacket {
    pub call_id: u64,
    pub sender_id: u32,
    pub seq: u32,
    pub is_keepalive: bool,
    pub payload: Vec<u8>,
}

impl MediaPacket {
    pub fn new(call_id: u64, sender_id: u32, seq: u32, payload: Vec<u8>) -> Self {
        Self {
            call_id,
            sender_id,
            seq,
            is_keepalive: payload.is_empty(),
            payload,
        }
    }

    pub fn keepalive(call_id: u64, sender_id: u32) -> Self {
        Self {
            call_id,
            sender_id,
            seq: 0,
            is_keepalive: true,
            payload: Vec::new(),
        }
    }

    /// Сериализация заголовок+payload в один буфер
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MEDIA_WIRE_HEADER_LEN + self.payload.len());
        out.extend_from_slice(&MEDIA_MAGIC.to_le_bytes());
        out.push(MEDIA_VERSION);
        out.push(if self.is_keepalive { FLAG_KEEPALIVE } else { 0 });
        out.extend_from_slice(&self.call_id.to_le_bytes());
        out.extend_from_slice(&self.sender_id.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Зашифрованная сериализация (ChaCha20-Poly1305)
    pub fn encode_encrypted(&self, key: &[u8; 32]) -> Vec<u8> {
        let payload_enc = if self.is_keepalive || self.payload.is_empty() {
            Vec::new()
        } else {
            encrypt_media_payload(key, self.call_id, self.seq, &self.payload)
                .unwrap_or_else(|_| Vec::new())
        };

        let mut out = Vec::with_capacity(MEDIA_WIRE_HEADER_LEN + payload_enc.len());
        out.extend_from_slice(&MEDIA_MAGIC.to_le_bytes());
        out.push(MEDIA_VERSION);
        out.push(if self.is_keepalive { FLAG_KEEPALIVE } else { 0 });
        out.extend_from_slice(&self.call_id.to_le_bytes());
        out.extend_from_slice(&self.sender_id.to_le_bytes());
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&payload_enc);
        out
    }

    /// Разбор пакета из буфера; None если пакет не наш/битый
    pub fn decode(bytes: &[u8]) -> Option<MediaPacket> {
        if bytes.len() < MEDIA_WIRE_HEADER_LEN {
            return None;
        }
        let magic = u16::from_le_bytes([bytes[0], bytes[1]]);
        if magic != MEDIA_MAGIC || bytes[2] != MEDIA_VERSION {
            return None;
        }
        let flags = bytes[3];
        let call_id = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
        let sender_id = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        let seq = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
        Some(MediaPacket {
            call_id,
            sender_id,
            seq,
            is_keepalive: flags & FLAG_KEEPALIVE != 0,
            payload: bytes[MEDIA_WIRE_HEADER_LEN..].to_vec(),
        })
    }

    /// Зашифрованный разбор и расшифровка пакета (ChaCha20-Poly1305)
    pub fn decode_encrypted(bytes: &[u8], key: &[u8; 32]) -> Option<MediaPacket> {
        let raw = Self::decode(bytes)?;
        if raw.is_keepalive || raw.payload.is_empty() {
            return Some(raw);
        }
        let decrypted_payload = decrypt_media_payload(key, raw.call_id, raw.seq, &raw.payload).ok()?;
        Some(MediaPacket {
            call_id: raw.call_id,
            sender_id: raw.sender_id,
            seq: raw.seq,
            is_keepalive: raw.is_keepalive,
            payload: decrypted_payload,
        })
    }
}

pub fn derive_media_key(call_id: u64) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"CHEBURGRAM_MEDIA_KEY_V3:");
    hasher.update(&call_id.to_le_bytes());
    let res = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&res);
    key
}

pub fn derive_media_nonce(call_id: u64, seq: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[0..8].copy_from_slice(&call_id.to_le_bytes());
    nonce[8..12].copy_from_slice(&seq.to_le_bytes());
    nonce
}

pub fn encrypt_media_payload(key: &[u8; 32], call_id: u64, seq: u32, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{aead::{Aead, KeyInit}, ChaCha20Poly1305, Key, Nonce};
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce_bytes = derive_media_nonce(call_id, seq);
    let nonce = Nonce::from_slice(&nonce_bytes);
    cipher.encrypt(nonce, plaintext).map_err(|e| e.to_string())
}

pub fn decrypt_media_payload(key: &[u8; 32], call_id: u64, seq: u32, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
    use chacha20poly1305::{aead::{Aead, KeyInit}, ChaCha20Poly1305, Key, Nonce};
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let nonce_bytes = derive_media_nonce(call_id, seq);
    let nonce = Nonce::from_slice(&nonce_bytes);
    cipher.decrypt(nonce, ciphertext).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_message_bincode_roundtrip() {
        let messages = vec![
            ControlMessage::Hello { protocol_version: 3 },
            ControlMessage::Challenge { nonce: [0x42; 32] },
            ControlMessage::Register { client_id: "id1".into(), user_code: "123456".into(), name: "User".into() },
            ControlMessage::Registered { peer_id: 1, user_code: "123456".into(), auth_token: Some("secret_token".into()), udp_port: 7879 },
            ControlMessage::Auth { user_code: "123456".into(), proof: [0xAA; 32] },
            ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { peer_id: 1, user_code: "123456".into(), udp_port: 7879, auth_token: None } },
            ControlMessage::AuthResponse { outcome: AuthOutcome::Failed(AuthError::InvalidToken) },
            ControlMessage::SessionReplaced,
            ControlMessage::SendFriendRequest { target_code: "654321".into() },
            ControlMessage::IncomingFriendRequest { from_code: "654321".into(), from_name: "Friend".into() },
            ControlMessage::FriendRequestAccepted { user_code: "654321".into(), name: "Friend".into() },
            ControlMessage::GetFriendsStatus { user_codes: vec!["654321".into()] },
            ControlMessage::FriendsStatus { friends: vec![FriendStatus { user_code: "654321".into(), name: "F".into(), is_online: true, peer_id: Some(2) }] },
            ControlMessage::SendTextMessage { target_code: "654321".into(), text: "Привет".into(), message_id: "m1".into() },
            ControlMessage::IncomingTextMessage { msg: TextMessage { id: "m1".into(), from_code: "654321".into(), from_name: "F".into(), to_code: "123456".into(), text: "Привет".into(), timestamp: "2026-01-01T00:00:00Z".into() } },
            ControlMessage::TextMessageAck { message_id: "m1".into(), delivered: true },
            ControlMessage::UserStatusChanged { user_code: "654321".into(), is_online: false, peer_id: None },
            ControlMessage::CallRequest { target_code: "654321".into() },
            ControlMessage::IncomingCall { from_code: "654321".into(), from_name: "F".into(), from_peer_id: 2 },
            ControlMessage::CallAccept { target_peer_id: 2 },
            ControlMessage::CallAccepted { peer_id: 2, peer_name: "F".into(), call_id: 100 },
            ControlMessage::CallReject { target_peer_id: 2 },
            ControlMessage::CallRejected { peer_id: 2, peer_name: "F".into() },
            ControlMessage::CallEnd,
            ControlMessage::CallEnded { peer_name: "F".into() },
            ControlMessage::CallMissed { peer_name: "F".into() },
            ControlMessage::Ping,
            ControlMessage::Pong,
            ControlMessage::Error { message: "Fail".into() },
        ];
        for msg in messages {
            let frame = encode_frame(&msg).unwrap();
            let len = u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize;
            assert_eq!(len, frame.len() - 4);
            let decoded = decode_payload(&frame[4..]).unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn test_frame_sync_io() {
        let msg = ControlMessage::CallAccepted { peer_id: 7, peer_name: "Тест".into(), call_id: 42 };
        let mut buf = Vec::new();
        write_frame_sync(&mut buf, &msg).unwrap();
        let mut slice = &buf[..];
        let decoded = read_frame_sync(&mut slice).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn test_frame_rejects_garbage_len() {
        let garbage = u32::MAX.to_le_bytes().to_vec();
        let mut slice = &garbage[..];
        assert!(read_frame_sync(&mut slice).is_err());
    }

    #[test]
    fn test_media_packet_roundtrip() {
        let pkt = MediaPacket::new(12345, 99, 500, vec![10, 20, 30, 40, 50]);
        let bytes = pkt.encode();
        assert_eq!(bytes.len(), MEDIA_WIRE_HEADER_LEN + 5);
        let decoded = MediaPacket::decode(&bytes).unwrap();
        assert_eq!(pkt, decoded);
        assert_eq!(decoded.call_id, 12345);
        assert_eq!(decoded.sender_id, 99);
        assert_eq!(decoded.seq, 500);
        assert!(!decoded.is_keepalive);
    }

    #[test]
    fn test_media_packet_keepalive() {
        let pkt = MediaPacket::keepalive(777, 42);
        let bytes = pkt.encode();
        let decoded = MediaPacket::decode(&bytes).unwrap();
        assert!(decoded.is_keepalive);
        assert!(decoded.payload.is_empty());
    }

    #[test]
    fn test_media_packet_rejects_garbage() {
        assert!(MediaPacket::decode(&[]).is_none());
        assert!(MediaPacket::decode(&[0u8; 10]).is_none());
        assert!(MediaPacket::decode(b"GARBAGEGARBAGEGARBAGE12").is_none());
    }

    #[test]
    fn test_media_vs_json_size() {
        // 20-байтовый заголовок против ~400 байт JSON у v1
        let pkt = MediaPacket::new(u64::MAX, u32::MAX, u32::MAX, vec![0u8; 100]);
        assert_eq!(pkt.encode().len(), 120);
    }

    #[test]
    fn test_challenge_response_crypto() {
        let token = "super_secret_token_12345";
        let token_hash = compute_token_hash(token);
        let nonce = [0x42u8; 32];

        let proof1 = compute_auth_proof(&token_hash, &nonce);
        let proof2 = compute_auth_proof(&token_hash, &nonce);
        assert!(constant_time_eq(&proof1, &proof2));

        let wrong_token_hash = compute_token_hash("wrong_token");
        let wrong_proof = compute_auth_proof(&wrong_token_hash, &nonce);
        assert!(!constant_time_eq(&proof1, &wrong_proof));
    }

    #[test]
    fn test_media_packet_encryption() {
        let key = derive_media_key(99999);
        let pkt = MediaPacket::new(99999, 42, 100, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        
        let enc_bytes = pkt.encode_encrypted(&key);
        // Header 20 bytes + payload 8 bytes + 16 bytes Poly1305 tag = 44 bytes
        assert_eq!(enc_bytes.len(), MEDIA_WIRE_HEADER_LEN + 8 + 16);

        let dec_pkt = MediaPacket::decode_encrypted(&enc_bytes, &key).unwrap();
        assert_eq!(pkt, dec_pkt);

        let wrong_key = derive_media_key(88888);
        assert!(MediaPacket::decode_encrypted(&enc_bytes, &wrong_key).is_none());
    }
}

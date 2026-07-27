//! Cheburgram Protocol v2
//!
//! Отличия от v1 (JSON):
//! - TCP-сигналинг: бинарные кадры [u32 LE длина][bincode payload]
//! - UDP-медиа: фиксированный заголовок 20 байт + Opus payload
//! - Heartbeat Ping/Pong и SessionReplaced для корректной замены сессий

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 2;
pub const MEDIA_MAGIC: u16 = 0x4347; // "CG"
pub const MEDIA_VERSION: u8 = 2;
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

// ─── Сигнальные сообщения (TCP) ─────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ControlMessage {
    // === Рукопожатие и регистрация ===
    Hello {
        protocol_version: u16,
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
        udp_port: u16,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_message_bincode_roundtrip() {
        let messages = vec![
            ControlMessage::Hello { protocol_version: 2 },
            ControlMessage::Register { client_id: "id1".into(), user_code: "123456".into(), name: "User".into() },
            ControlMessage::Registered { peer_id: 1, user_code: "123456".into(), udp_port: 7879 },
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
}

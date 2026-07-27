use serde::{Deserialize, Serialize};

/// Информация о друге/контакте
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FriendStatus {
    pub user_code: String,
    pub name: String,
    pub is_online: bool,
    pub peer_id: Option<u32>,
}

/// Запрос в друзья
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FriendRequestInfo {
    pub from_code: String,
    pub from_name: String,
}

/// Текстовое сообщение (SMS / Чат)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TextMessage {
    pub id: String,
    pub from_code: String,
    pub from_name: String,
    pub to_code: String,
    pub text: String,
    pub timestamp: String, // ISO 8601
}

/// Запись о звонке (для истории)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallRecord {
    pub peer_name: String,
    pub direction: CallDirection,
    pub timestamp: String, // ISO 8601
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CallDirection {
    Incoming,
    Outgoing,
    Missed,
}

/// Все сигнальные и управляющие сообщения (TCP)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ControlMessage {
    // === Регистрация ===
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

    // === Друзья и Запросы ===
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

    // === Текстовые сообщения (SMS / Чат) ===
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

    // === Уведомления присутствия ===
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

    // === Служебные сообщения ===
    Ping,
    Pong,
    Error {
        message: String,
    },
}

/// UDP аудио пакет
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioPacketHeader {
    pub room_id: u64,
    pub sender_id: u32,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioPacket {
    pub header: AudioPacketHeader,
    pub payload: Vec<u8>,
}

impl AudioPacket {
    pub fn new(room_id: u64, sender_id: u32, sequence: u64, payload: Vec<u8>) -> Self {
        Self {
            header: AudioPacketHeader { room_id, sender_id, sequence },
            payload,
        }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_control_messages_serde() {
        let messages = vec![
            ControlMessage::Register { client_id: "id1".into(), user_code: "123456".into(), name: "User".into() },
            ControlMessage::Registered { peer_id: 1, user_code: "123456".into(), udp_port: 7879 },
            ControlMessage::SendFriendRequest { target_code: "654321".into() },
            ControlMessage::IncomingFriendRequest { from_code: "654321".into(), from_name: "Friend".into() },
            ControlMessage::AcceptFriendRequest { from_code: "654321".into() },
            ControlMessage::RejectFriendRequest { from_code: "654321".into() },
            ControlMessage::FriendRequestAccepted { user_code: "654321".into(), name: "Friend".into() },
            ControlMessage::GetFriendsStatus { user_codes: vec!["654321".into()] },
            ControlMessage::FriendsStatus { friends: vec![FriendStatus { user_code: "654321".into(), name: "Friend".into(), is_online: true, peer_id: Some(2) }] },
            ControlMessage::PendingFriendRequests { requests: vec![FriendRequestInfo { from_code: "654321".into(), from_name: "Friend".into() }] },
            ControlMessage::SendTextMessage { target_code: "654321".into(), text: "Hello".into(), message_id: "m1".into() },
            ControlMessage::IncomingTextMessage { msg: TextMessage { id: "m1".into(), from_code: "654321".into(), from_name: "Friend".into(), to_code: "123456".into(), text: "Hello".into(), timestamp: "2026-01-01T00:00:00Z".into() } },
            ControlMessage::TextMessageAck { message_id: "m1".into(), delivered: true },
            ControlMessage::PendingTextMessages { messages: vec![] },
            ControlMessage::UserStatusChanged { user_code: "654321".into(), is_online: false, peer_id: None },
            ControlMessage::CallRequest { target_code: "654321".into() },
            ControlMessage::IncomingCall { from_code: "654321".into(), from_name: "Friend".into(), from_peer_id: 2 },
            ControlMessage::CallAccept { target_peer_id: 2 },
            ControlMessage::CallAccepted { peer_id: 2, peer_name: "Friend".into(), call_id: 100 },
            ControlMessage::CallReject { target_peer_id: 2 },
            ControlMessage::CallRejected { peer_id: 2, peer_name: "Friend".into() },
            ControlMessage::CallEnd,
            ControlMessage::CallEnded { peer_name: "Friend".into() },
            ControlMessage::Ping,
            ControlMessage::Pong,
            ControlMessage::Error { message: "Fail".into() },
        ];

        for msg in messages {
            let json = serde_json::to_string(&msg).unwrap();
            let decoded: ControlMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(msg, decoded);
        }
    }

    #[test]
    fn test_audio_packet_serde_and_bytes() {
        let pkt = AudioPacket::new(12345, 99, 500, vec![10, 20, 30, 40, 50]);
        let bytes = pkt.to_bytes().unwrap();
        let decoded = AudioPacket::from_bytes(&bytes).unwrap();
        assert_eq!(pkt, decoded);
        assert_eq!(decoded.header.room_id, 12345);
        assert_eq!(decoded.header.sender_id, 99);
        assert_eq!(decoded.header.sequence, 500);
        assert_eq!(decoded.payload, vec![10, 20, 30, 40, 50]);
    }
}

use serde::{Deserialize, Serialize};

/// Статус друга
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendStatus {
    pub user_code: String,
    pub name: String,
    pub is_online: bool,
    pub peer_id: Option<u32>,
}

/// Запрос в друзья
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendRequestInfo {
    pub from_code: String,
    pub from_name: String,
}

/// Запись о звонке (для истории)
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Все сигнальные сообщения (TCP)
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    // === Служебное ===
    Ping,
    Pong,
    Error {
        message: String,
    },
}

/// UDP аудио пакет
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPacketHeader {
    pub room_id: u64,
    pub sender_id: u32,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

use serde::{Deserialize, Serialize};

/// Статус друга
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendStatus {
    pub user_code: String,
    pub name: String,
    pub is_online: bool,
    pub peer_id: Option<u32>,
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
    /// Клиент регистрируется с постоянными client_id, user_code (6 цифр) и отображаемым именем
    Register {
        client_id: String,
        user_code: String,
        name: String,
    },
    /// Сервер подтверждает регистрацию
    Registered {
        peer_id: u32,
        user_code: String,
        udp_port: u16,
    },

    // === Друзья и Поиск ===
    /// Поиск пользователя по 6-значному ID
    LookupUser {
        user_code: String,
    },
    /// Результат поиска
    UserLookupResult {
        found: bool,
        user_code: String,
        name: String,
        is_online: bool,
        peer_id: Option<u32>,
    },
    /// Запрос статуса списка друзей
    GetFriendsStatus {
        user_codes: Vec<String>,
    },
    /// Ответ со статусом друзей
    FriendsStatus {
        friends: Vec<FriendStatus>,
    },

    // === Уведомления присутствия ===
    UserStatusChanged {
        user_code: String,
        is_online: bool,
        peer_id: Option<u32>,
    },

    // === Звонки ===
    /// Исходящий звонок по user_code или peer_id
    CallRequest {
        target_code: String,
    },
    /// Входящий звонок целевому клиенту
    IncomingCall {
        from_code: String,
        from_name: String,
        from_peer_id: u32,
    },
    /// Принять звонок
    CallAccept {
        target_peer_id: u32,
    },
    /// Звонок принят
    CallAccepted {
        peer_id: u32,
        peer_name: String,
    },
    /// Отклонить звонок
    CallReject {
        target_peer_id: u32,
    },
    /// Звонок отклонён
    CallRejected {
        peer_id: u32,
        peer_name: String,
    },
    /// Завершить звонок
    CallEnd,
    /// Уведомление о завершении звонка
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

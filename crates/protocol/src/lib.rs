use serde::{Deserialize, Serialize};

/// Информация о пользователе
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub peer_id: u32,
    pub name: String,
}

/// Запись о звонке (для истории)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    pub peer_name: String,
    pub direction: CallDirection,
    pub timestamp: String, // ISO 8601
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CallDirection {
    Incoming,
    Outgoing,
    Missed,
}

/// Все сигнальные сообщения (TCP)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    // === Регистрация ===
    /// Клиент регистрируется: client_id = UUID (постоянный), name = отображаемое имя
    Register {
        client_id: String,
        name: String,
    },
    /// Сервер подтверждает регистрацию
    Registered {
        peer_id: u32,
        udp_port: u16,
    },

    // === Присутствие (онлайн/офлайн) ===
    /// Полный список пользователей (отправляется при входе)
    UserList {
        users: Vec<UserInfo>,
    },
    /// Новый пользователь появился онлайн
    UserOnline {
        peer_id: u32,
        name: String,
    },
    /// Пользователь ушёл офлайн
    UserOffline {
        peer_id: u32,
        name: String,
    },

    // === Звонки ===
    /// Исходящий запрос на звонок (клиент → сервер, сервер → цель)
    CallRequest {
        to_id: u32,
    },
    /// Сервер уведомляет цель о входящем звонке
    IncomingCall {
        from_id: u32,
        from_name: String,
    },
    /// Цель принимает звонок
    CallAccept {
        to_id: u32,
    },
    /// Сервер уведомляет инициатора что звонок принят
    CallAccepted {
        peer_id: u32,
        peer_name: String,
    },
    /// Звонок отклонён
    CallReject {
        to_id: u32,
    },
    /// Сервер уведомляет инициатора что звонок отклонён
    CallRejected {
        peer_id: u32,
        peer_name: String,
    },
    /// Завершить активный звонок (любой участник)
    CallEnd,
    /// Уведомление что звонок завершён другой стороной
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
    pub room_id: u64,   // уникальный ID сессии звонка
    pub sender_id: u32,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPacket {
    pub header: AudioPacketHeader,
    pub payload: Vec<u8>, // Opus-сжатый фрейм (пустой = ping-регистрация)
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

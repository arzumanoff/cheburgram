use serde::{Deserialize, Serialize};

/// Сообщения сигнализации (TCP контрольное соединение)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Запрос на создание новой комнаты
    CreateRoom,
    /// Ответ сервера: комната создана
    RoomCreated {
        room_code: String,
        peer_id: u32,
        udp_port: u16,
    },
    /// Запрос на подключение к существующей комнате
    JoinRoom {
        room_code: String,
    },
    /// Ответ сервера: успешный вход в комнату
    RoomJoined {
        room_code: String,
        peer_id: u32,
        udp_port: u16,
    },
    /// Уведомление: второй собеседник подключился к комнате
    PeerConnected {
        peer_id: u32,
    },
    /// Уведомление: второй собеседник отключился
    PeerDisconnected {
        peer_id: u32,
    },
    /// Пинг / Понг для проверки задержки
    Ping,
    Pong,
    /// Сообщение об ошибке
    Error {
        message: String,
    },
}

/// Заголовок UDP пакета с голосом
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPacketHeader {
    pub room_code: String,
    pub sender_id: u32,
    pub sequence: u64,
    pub timestamp_ms: u64,
}

/// Полный зашифрованный/передаваемый пакет аудио
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPacket {
    pub header: AudioPacketHeader,
    pub payload: Vec<u8>, // Сжатый Opus фрейм
}

impl AudioPacket {
    pub fn new(room_code: String, sender_id: u32, sequence: u64, timestamp_ms: u64, payload: Vec<u8>) -> Self {
        Self {
            header: AudioPacketHeader {
                room_code,
                sender_id,
                sequence,
                timestamp_ms,
            },
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

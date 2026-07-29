//! Cheburgram Audio Engine
//!
//! Цепочка захвата:  mic → mono f32 → ресемплинг → 48 кГц → Opus (FEC) → UDP
//! Цепочка вывода:   UDP → jitter buffer → Opus (PLC/FEC) → ресемплинг → sum → device
//!
//! Все управляющие параметры (мьюты, громкость) — атомики, читаемые каждый кадр,
//! поэтому применяются мгновенно, без пересоздания потоков.

pub mod devices;
pub mod engine;
pub mod jitter;
pub mod resample;
pub mod ringtone;

pub use engine::{start_call_audio, AudioHandle, AudioStats, CallAudioConfig};
pub use jitter::{JitterBuffer, Pop};
pub use ringtone::{start_ringtone, RingtoneHandle};

pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SIZE: usize = 960; // 20 мс при 48 кГц
pub const OPUS_BITRATE: i32 = 32_000;
pub const MAX_OPUS_BYTES: usize = 4000;

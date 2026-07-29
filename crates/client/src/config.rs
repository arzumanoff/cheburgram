//! Конфигурация клиента (%APPDATA%\Cheburgram\config.json / ~/.local/share/cheburgram)

use cheburgram_protocol::CallRecord;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use uuid::Uuid;

pub const DEFAULT_SERVER: &str = "127.0.0.1:7880";

fn default_zoom() -> f32 {
    1.0
}
fn default_server_address() -> String {
    DEFAULT_SERVER.to_string()
}
fn default_user_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    format!("{:06}", rng.gen_range(100_000..999_999))
}
fn default_tls_enabled() -> bool {
    true
}
fn default_mic_gain() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default = "default_user_code")]
    pub user_code: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default = "default_server_address")]
    pub server_address: String,
    #[serde(default)]
    pub selected_input: usize,
    #[serde(default)]
    pub selected_output: usize,
    #[serde(default = "default_zoom")]
    pub zoom_factor: f32,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub server_fingerprint: Option<String>,
    #[serde(default = "default_tls_enabled")]
    pub tls_enabled: bool,
    #[serde(default = "default_tls_enabled")]
    pub dark_mode: bool,
    #[serde(default = "default_mic_gain")]
    pub mic_gain: f32,
    #[serde(default)]
    pub friends: Vec<SavedFriend>,
    #[serde(default)]
    pub call_history: Vec<CallRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedFriend {
    pub user_code: String,
    pub name: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            client_id: Uuid::new_v4().to_string(),
            user_code: default_user_code(),
            display_name: String::new(),
            server_address: DEFAULT_SERVER.to_string(),
            selected_input: 0,
            selected_output: 0,
            zoom_factor: 1.0,
            auth_token: None,
            server_fingerprint: None,
            tls_enabled: true,
            dark_mode: true,
            mic_gain: 1.0,
            friends: Vec::new(),
            call_history: Vec::new(),
        }
    }
}

fn config_path() -> PathBuf {
    let base = if let Ok(ap) = std::env::var("APPDATA") {
        PathBuf::from(ap).join("Cheburgram")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/cheburgram")
    } else {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
    };
    fs::create_dir_all(&base).ok();
    base.join("config.json")
}

pub fn load_config() -> AppConfig {
    let p = config_path();
    if p.exists() {
        if let Ok(d) = fs::read_to_string(&p) {
            if let Ok(mut c) = serde_json::from_str::<AppConfig>(&d) {
                if c.client_id.is_empty() {
                    c.client_id = Uuid::new_v4().to_string();
                }
                if c.user_code.len() != 6 || !c.user_code.chars().all(|ch| ch.is_ascii_digit()) {
                    c.user_code = default_user_code();
                }
                c.server_address = normalize_server(&c.server_address);
                save_config(&c);
                return c;
            }
        }
    }
    let c = AppConfig::default();
    save_config(&c);
    c
}

pub fn save_config(c: &AppConfig) {
    if let Ok(d) = serde_json::to_string_pretty(c) {
        let tmp = config_path().with_extension("tmp");
        if fs::write(&tmp, d).is_ok() {
            let _ = fs::rename(&tmp, config_path());
        }
    }
}

/// Нормализация адреса сервера: "host" → "host:7880"
pub fn normalize_server(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return DEFAULT_SERVER.to_string();
    }
    if s.ends_with(":22") {
        return format!("{}:7880", s.trim_end_matches(":22"));
    }
    if s.ends_with(":7878") {
        return format!("{}:7880", s.trim_end_matches(":7878"));
    }
    if !s.contains(':') {
        return format!("{}:7880", s);
    }
    s.to_string()
}

/// Хост без порта — для UDP-цели
pub fn server_host(s: &str) -> String {
    normalize_server(s)
        .split(':')
        .next()
        .unwrap_or("127.0.0.1")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize() {
        assert_eq!(normalize_server(""), DEFAULT_SERVER);
        assert_eq!(normalize_server("  "), DEFAULT_SERVER);
        assert_eq!(normalize_server("1.2.3.4"), "1.2.3.4:7880");
        assert_eq!(normalize_server("my-vps.com"), "my-vps.com:7880");
        assert_eq!(normalize_server("my-vps.com:9090"), "my-vps.com:9090");
        assert_eq!(normalize_server("my-vps.com:22"), "my-vps.com:7880");
    }

    #[test]
    fn test_host() {
        assert_eq!(server_host("1.2.3.4:7880"), "1.2.3.4");
        assert_eq!(server_host("my-vps.com"), "my-vps.com");
    }

    #[test]
    fn test_config_serde() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.user_code.len(), 6);
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: AppConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.user_code, decoded.user_code);
        assert_eq!(cfg.client_id, decoded.client_id);
    }

    #[test]
    fn test_old_config_without_token_parses() {
        let old_json = r#"{
            "client_id": "abc-123-xyz",
            "user_code": "654321",
            "display_name": "Борис",
            "server_address": "127.0.0.1:7878"
        }"#;

        let decoded: AppConfig = serde_json::from_str(old_json).unwrap();
        assert_eq!(decoded.client_id, "abc-123-xyz");
        assert_eq!(decoded.user_code, "654321");
        assert_eq!(decoded.display_name, "Борис");
        assert!(decoded.auth_token.is_none());
    }
}

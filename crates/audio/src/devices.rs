//! Перечисление и открытие аудиоустройств с fallback на устройство по умолчанию.

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Device;
use tracing::warn;

pub const DEFAULT_DEVICE_LABEL: &str = "По умолчанию";

/// Список имён устройств: первым всегда "По умолчанию"
pub fn list_inputs() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = vec![DEFAULT_DEVICE_LABEL.to_string()];
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(n) = d.name() {
                names.push(n);
            }
        }
    }
    names
}

pub fn list_outputs() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = vec![DEFAULT_DEVICE_LABEL.to_string()];
    if let Ok(devs) = host.output_devices() {
        for d in devs {
            if let Ok(n) = d.name() {
                names.push(n);
            }
        }
    }
    names
}

/// Открыть входное устройство по имени; при любой проблеме — устройство по умолчанию
pub fn open_input(name: Option<&str>) -> Option<Device> {
    let host = cpal::default_host();
    if let Some(n) = name.filter(|n| !n.is_empty() && *n != DEFAULT_DEVICE_LABEL) {
        if let Ok(mut devs) = host.input_devices() {
            if let Some(d) = devs.find(|d| d.name().ok().as_deref() == Some(n)) {
                return Some(d);
            }
        }
        warn!("Микрофон '{}' не найден, используем устройство по умолчанию", n);
    }
    host.default_input_device()
}

/// Открыть выходное устройство по имени; при любой проблеме — устройство по умолчанию
pub fn open_output(name: Option<&str>) -> Option<Device> {
    let host = cpal::default_host();
    if let Some(n) = name.filter(|n| !n.is_empty() && *n != DEFAULT_DEVICE_LABEL) {
        if let Ok(mut devs) = host.output_devices() {
            if let Some(d) = devs.find(|d| d.name().ok().as_deref() == Some(n)) {
                return Some(d);
            }
        }
        warn!("Устройство вывода '{}' не найдено, используем по умолчанию", n);
    }
    host.default_output_device()
}

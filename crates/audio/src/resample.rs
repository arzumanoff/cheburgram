//! Ресемплинг mono f32 потока через rubato (sinc-интерполяция).
//! При совпадении частот — прозрачный passthrough без лишнего копирования.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

const SINC_CHUNK: usize = 1024;

fn sinc_params() -> SincInterpolationParameters {
    SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    }
}

/// Конвертер частоты для моно f32 потока.
/// Принимает произвольные порции через staging-буфер, отдаёт готовые сэмплы выходной частоты.
pub struct RateConverter {
    stage: Vec<f32>,
    inner: Option<SincFixedIn<f32>>,
}

impl RateConverter {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        if from_rate == to_rate {
            return Self { stage: Vec::new(), inner: None };
        }
        let inner = SincFixedIn::<f32>::new(
            to_rate as f64 / from_rate as f64,
            2.0,
            sinc_params(),
            SINC_CHUNK,
            1,
        )
        .ok();
        Self { stage: Vec::with_capacity(SINC_CHUNK * 2), inner }
    }

    /// Скармливает порцию входных сэмплов, возвращает выходные (может быть пусто —
    /// ресемплер накапливает внутреннюю задержку)
    pub fn push(&mut self, input: &[f32]) -> Vec<f32> {
        let Some(resampler) = self.inner.as_mut() else {
            return input.to_vec();
        };
        self.stage.extend_from_slice(input);
        let mut out = Vec::new();
        while self.stage.len() >= SINC_CHUNK {
            let chunk: Vec<f32> = self.stage.drain(..SINC_CHUNK).collect();
            match resampler.process(&[chunk], None) {
                Ok(mut frames) => {
                    if let Some(ch) = frames.pop() {
                        out.extend_from_slice(&ch);
                    }
                }
                Err(_) => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_passthrough_same_rate() {
        let mut c = RateConverter::new(48_000, 48_000);
        let input: Vec<f32> = (0..960).map(|i| i as f32 / 960.0).collect();
        let out = c.push(&input);
        assert_eq!(out.len(), 960);
        assert!((out[500] - input[500]).abs() < f32::EPSILON);
    }

    #[test]
    fn test_upsample_16k_to_48k() {
        let mut c = RateConverter::new(16_000, 48_000);
        // кормим синусоиду; за 0.5 сек должно выйти ~48000*0.5 сэмплов
        let input: Vec<f32> = (0..8000)
            .map(|i| (i as f32 * 440.0 * 2.0 * std::f32::consts::PI / 16000.0).sin())
            .collect();
        let mut total = 0usize;
        for chunk in input.chunks(SINC_CHUNK) {
            total += c.push(chunk).len();
        }
        // sinc задерживает начало, допускаем -2 блока
        assert!(total > 24_000 - 2 * 3 * SINC_CHUNK, "total={}", total);
        assert!(total <= 24_000 + 3 * SINC_CHUNK, "total={}", total);
    }

    #[test]
    fn test_downsample_44100_to_48k_rate_close() {
        let mut c = RateConverter::new(44_100, 48_000);
        let input: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.01).sin()).collect();
        let mut total = 0usize;
        for chunk in input.chunks(SINC_CHUNK) {
            total += c.push(chunk).len();
        }
        // 1 секунда входа -> ~48000 выхода (±3 блока на задержку)
        assert!(total > 48_000 - 3 * 3 * SINC_CHUNK, "total={}", total);
        assert!(total < 48_000 + 3 * SINC_CHUNK, "total={}", total);
    }
}

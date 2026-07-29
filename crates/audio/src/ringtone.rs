use crate::devices;
use cpal::traits::{DeviceTrait, StreamTrait};
use minimp3::{Decoder, Error, Frame};
use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::{error, info};

pub struct RingtoneHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for RingtoneHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn load_mp3_samples(path: &str) -> Option<(Vec<f32>, u32, usize)> {
    let file = File::open(path).ok()?;
    let mut decoder = Decoder::new(file);
    let mut pcm: Vec<f32> = Vec::new();
    let mut sample_rate = 44100u32;
    let mut channels = 2usize;

    loop {
        match decoder.next_frame() {
            Ok(Frame {
                data,
                sample_rate: sr,
                channels: ch,
                ..
            }) => {
                sample_rate = sr as u32;
                channels = ch;
                for s in data {
                    pcm.push(s as f32 / 32768.0);
                }
            }
            Err(Error::Eof) => break,
            Err(_) => break,
        }
    }
    if pcm.is_empty() {
        None
    } else {
        Some((pcm, sample_rate, channels))
    }
}

pub fn start_ringtone(custom_path: Option<&str>, dev_name: Option<String>) -> RingtoneHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_cloned = stop.clone();

    let target_path = custom_path.unwrap_or("call.mp3").to_string();

    let thread = thread::spawn(move || {
        let (samples, _sr, _ch) = load_mp3_samples(&target_path)
            .or_else(|| load_mp3_samples("E:\\CHEBURGRAM\\call.mp3"))
            .unwrap_or_else(|| {
                // Запасной генератор мелодии если файл не найден
                let sr = 44100u32;
                let ch = 1usize;
                let mut pcm = Vec::with_capacity((sr * 2) as usize);
                for i in 0..(sr * 2) {
                    let t = i as f32 / sr as f32;
                    let tone = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.3;
                    pcm.push(tone);
                }
                (pcm, sr, ch)
            });

        let device = match devices::open_output(dev_name.as_deref()) {
            Some(d) => d,
            None => return,
        };
        let dc = match device.default_output_config() {
            Ok(c) => c,
            Err(_) => return,
        };
        let config: cpal::StreamConfig = dc.into();

        let mut idx = 0usize;
        let samples_len = samples.len();
        if samples_len == 0 {
            return;
        }

        let stop_cb = stop_cloned.clone();
        let err_fn = |e: cpal::StreamError| error!("cpal ringtone: {}", e);

        let stream = match device.build_output_stream(
            &config,
            move |data: &mut [f32], _| {
                for sample in data.iter_mut() {
                    if stop_cb.load(Ordering::Relaxed) {
                        *sample = 0.0;
                    } else {
                        *sample = samples[idx % samples_len];
                        idx = (idx + 1) % samples_len;
                    }
                }
            },
            err_fn,
            None,
        ) {
            Ok(s) => s,
            Err(e) => {
                error!("Рингтон stream build error: {:?}", e);
                return;
            }
        };

        let _ = stream.play();
        info!("Рингтон запущен ({})", target_path);

        while !stop_cloned.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(50));
        }
    });

    RingtoneHandle {
        stop,
        thread: Some(thread),
    }
}

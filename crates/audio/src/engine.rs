//! Движок звонка: захват, воспроизведение, keepalive.
//!
//! Потоки:
//! - keepalive  — пустой медиапакет каждые 2 с (регистрация UDP-адреса на релее)
//! - capture    — микрофон → 48k mono → Opus → UDP
//! - playback   — UDP → jitter → Opus decode (PLC/FEC) → mix → устройство вывода
//!
//! Ошибки устройств не роняют звонок: пишутся в `error`, остальные потоки живут.

use anyhow::Result;
use audiopus::coder::{Decoder, Encoder};
use audiopus::{Application, Bitrate, Channels, SampleRate};
use cheburgram_protocol::MediaPacket;
use cpal::traits::{DeviceTrait, StreamTrait};
use ringbuf::HeapRb;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

use crate::devices;
use crate::jitter::{JitterBuffer, Pop};
use crate::resample::RateConverter;
use crate::{FRAME_SIZE, MAX_OPUS_BYTES, OPUS_BITRATE, SAMPLE_RATE};

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(2);
const POP_INTERVAL: Duration = Duration::from_millis(20);
/// Стартовая подушка кольца воспроизведения (тишина), мс
const RING_PREROLL_MS: usize = 60;

#[derive(Clone)]
pub struct AudioStats {
    pub pkts_sent: Arc<AtomicU64>,
    pub pkts_recv: Arc<AtomicU64>,
    /// Сколько раз кольцо воспроизведения оказалось пустым (диагностика «скрипов»)
    pub underruns: Arc<AtomicU64>,
}

pub struct CallAudioConfig {
    /// Имя входного устройства (None / "По умолчанию" → default)
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub sock: Arc<UdpSocket>,
    pub target: SocketAddr,
    pub call_id: u64,
    pub my_peer_id: u32,
}

pub struct AudioHandle {
    pub stop: Arc<AtomicBool>,
    pub mic_muted: Arc<AtomicBool>,
    pub output_muted: Arc<AtomicBool>,
    pub mic_level: Arc<AtomicU8>,
    /// Усиление микрофона, f32 bits (0.5..3.0), по умолчанию 1.0
    pub mic_gain: Arc<AtomicU32>,
    /// Громкость собеседника, f32 bits (0.0..2.0), по умолчанию 1.0
    pub peer_volume: Arc<AtomicU32>,
    pub stats: AudioStats,
    /// Последняя аудио-ошибка для показа в UI
    pub error: Arc<Mutex<Option<String>>>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

pub fn start_call_audio(cfg: CallAudioConfig) -> AudioHandle {
    let mut handle = AudioHandle {
        stop: Arc::new(AtomicBool::new(false)),
        mic_muted: Arc::new(AtomicBool::new(false)),
        output_muted: Arc::new(AtomicBool::new(false)),
        mic_level: Arc::new(AtomicU8::new(0)),
        mic_gain: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        peer_volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        stats: AudioStats {
            pkts_sent: Arc::new(AtomicU64::new(0)),
            pkts_recv: Arc::new(AtomicU64::new(0)),
            underruns: Arc::new(AtomicU64::new(0)),
        },
        error: Arc::new(Mutex::new(None)),
        threads: Vec::new(),
    };

    // ── keepalive ──
    {
        let sock = cfg.sock.clone();
        let target = cfg.target;
        let call_id = cfg.call_id;
        let my_id = cfg.my_peer_id;
        let stop = handle.stop.clone();
        handle.threads.push(thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let pkt = MediaPacket::keepalive(call_id, my_id).encode();
                let _ = sock.send_to(&pkt, target);
                thread::sleep(KEEPALIVE_INTERVAL);
            }
        }));
    }

    // ── захват ──
    {
        let sock = cfg.sock.clone();
        let target = cfg.target;
        let call_id = cfg.call_id;
        let my_id = cfg.my_peer_id;
        let stop = handle.stop.clone();
        let mic_muted = handle.mic_muted.clone();
        let mic_level = handle.mic_level.clone();
        let pkts_sent = handle.stats.pkts_sent.clone();
        let err_out = handle.error.clone();
        let mic_gain = handle.mic_gain.clone();
        let dev_name = cfg.input_device.clone();
        handle.threads.push(thread::spawn(move || {
            if let Err(e) = run_capture(
                dev_name, sock, target, call_id, my_id, stop, mic_muted, mic_level, mic_gain, pkts_sent,
            ) {
                error!("Захват звука остановлен: {:?}", e);
                set_err(&err_out, format!("Микрофон: {}", e));
            }
        }));
    }

    // ── воспроизведение ──
    {
        let sock = cfg.sock.clone();
        let call_id = cfg.call_id;
        let stop = handle.stop.clone();
        let out_muted = handle.output_muted.clone();
        let volume = handle.peer_volume.clone();
        let pkts_recv = handle.stats.pkts_recv.clone();
        let underruns = handle.stats.underruns.clone();
        let err_out = handle.error.clone();
        let dev_name = cfg.output_device.clone();
        handle.threads.push(thread::spawn(move || {
            if let Err(e) =
                run_playback(dev_name, sock, call_id, stop, out_muted, volume, pkts_recv, underruns)
            {
                error!("Воспроизведение остановлено: {:?}", e);
                set_err(&err_out, format!("Динамики: {}", e));
            }
        }));
    }

    info!("Аудио звонка запущено (call_id={})", cfg.call_id);
    handle
}

fn set_err(slot: &Arc<Mutex<Option<String>>>, msg: String) {
    if let Ok(mut g) = slot.lock() {
        *g = Some(msg);
    }
}

fn make_encoder() -> Result<Encoder> {
    let mut enc = Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)?;
    enc.set_bitrate(Bitrate::BitsPerSecond(OPUS_BITRATE))?;
    enc.enable_inband_fec()?;
    enc.set_packet_loss_perc(20)?;
    Ok(enc)
}

fn make_decoder() -> Result<Decoder> {
    Ok(Decoder::new(SampleRate::Hz48000, Channels::Mono)?)
}

// ─── Захват ──────────────────────────────────────────────────────────────────

fn run_capture(
    dev_name: Option<String>,
    sock: Arc<UdpSocket>,
    target: SocketAddr,
    call_id: u64,
    my_id: u32,
    stop: Arc<AtomicBool>,
    mic_muted: Arc<AtomicBool>,
    mic_level: Arc<AtomicU8>,
    mic_gain: Arc<AtomicU32>,
    pkts_sent: Arc<AtomicU64>,
) -> Result<()> {
    let device = devices::open_input(dev_name.as_deref())
        .ok_or_else(|| anyhow::anyhow!("нет доступного микрофона"))?;
    let dc = device.default_input_config()?;
    let in_rate = dc.sample_rate().0;
    let channels = dc.channels() as usize;
    let fmt = dc.sample_format();
    let config: cpal::StreamConfig = dc.into();
    info!("Микрофон: {} Hz, {} ch, {:?}", in_rate, channels, fmt);

    let raw: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(FRAME_SIZE * 4)));
    let raw_cb = raw.clone();
    let push_mono = move |dst: &mut Vec<f32>, samples: &[f32]| {
        if channels == 1 {
            dst.extend_from_slice(samples);
        } else {
            for frame in samples.chunks(channels) {
                dst.push(frame.iter().sum::<f32>() / channels as f32);
            }
        }
    };

    let err_fn = |e: cpal::StreamError| error!("cpal capture: {}", e);
    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let raw2 = raw_cb.clone();
            device.build_input_stream(
                &config,
                move |d: &[f32], _| {
                    let mut b = raw2.lock().unwrap();
                    push_mono(&mut b, d);
                    // защита от переполнения при зависшем потребителе
                    if b.len() > SAMPLE_RATE as usize {
                        b.clear();
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let raw2 = raw_cb.clone();
            device.build_input_stream(
                &config,
                move |d: &[i16], _| {
                    let f: Vec<f32> = d.iter().map(|&s| s as f32 / 32768.0).collect();
                    let mut b = raw2.lock().unwrap();
                    push_mono(&mut b, &f);
                    if b.len() > SAMPLE_RATE as usize {
                        b.clear();
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let raw2 = raw_cb.clone();
            device.build_input_stream(
                &config,
                move |d: &[u16], _| {
                    let f: Vec<f32> = d
                        .iter()
                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                        .collect();
                    let mut b = raw2.lock().unwrap();
                    push_mono(&mut b, &f);
                    if b.len() > SAMPLE_RATE as usize {
                        b.clear();
                    }
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("формат захвата {:?} не поддерживается", other),
    };
    stream.play()?;

    let mut conv = RateConverter::new(in_rate, SAMPLE_RATE);
    let mut ready: Vec<f32> = Vec::with_capacity(FRAME_SIZE * 4);
    let mut enc = make_encoder()?;
    let mut obuf = vec![0u8; MAX_OPUS_BYTES];
    let mut seq: u32 = 1;
    let media_key = cheburgram_protocol::derive_media_key(call_id);
    let mut current_bitrate: i32 = OPUS_BITRATE;

    // RNNoise нейросетевое шумоподавление
    let mut denoiser = nnnoiseless::DenoiseState::new();

    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(5));
        let chunk: Vec<f32> = {
            let mut b = raw.lock().unwrap();
            std::mem::take(&mut *b)
        };
        if !chunk.is_empty() {
            ready.extend_from_slice(&conv.push(&chunk));
        }
        while ready.len() >= FRAME_SIZE {
            let raw_frame: Vec<f32> = ready.drain(..FRAME_SIZE).collect();

            // Адаптивная подстройка битрейта Opus каждые 200 кадров (~4 сек)
            if seq % 200 == 0 {
                let target_bitrate = if pkts_sent.load(Ordering::Relaxed) > 100 {
                    36_000
                } else {
                    OPUS_BITRATE
                };
                if target_bitrate != current_bitrate {
                    if enc.set_bitrate(Bitrate::BitsPerSecond(target_bitrate)).is_ok() {
                        current_bitrate = target_bitrate;
                    }
                }
            }

            // Подавление шума в двух 480-сэмпловых чанках (10мс)
            let mut clean_frame = vec![0.0f32; FRAME_SIZE];
            denoiser.process_frame(&mut clean_frame[0..480], &raw_frame[0..480]);
            denoiser.process_frame(&mut clean_frame[480..960], &raw_frame[480..960]);

            // Применение усиления микрофона
            let gain = f32::from_bits(mic_gain.load(Ordering::Relaxed));
            if (gain - 1.0).abs() > 0.01 {
                for s in clean_frame.iter_mut() {
                    *s = (*s * gain).clamp(-1.0, 1.0);
                }
            }

            let rms = (clean_frame.iter().map(|s| s * s).sum::<f32>() / clean_frame.len() as f32).sqrt();
            let lvl = ((rms * 6.0).sqrt() * 100.0).min(100.0) as u8;
            mic_level.store(lvl, Ordering::Relaxed);

            if mic_muted.load(Ordering::Relaxed) {
                continue; // DTX: не шлём и не тратим seq — у приёмника тишина без дыр
            }
            match enc.encode_float(&clean_frame, &mut obuf) {
                Ok(n) => {
                    let pkt = MediaPacket::new(call_id, my_id, seq, obuf[..n].to_vec()).encode_encrypted(&media_key);
                    let _ = sock.send_to(&pkt, target);
                    seq = seq.wrapping_add(1);
                    pkts_sent.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => warn!("Opus encode: {:?}", e),
            }
        }
    }
    mic_level.store(0, Ordering::Relaxed);
    Ok(())
}

// ─── Воспроизведение ─────────────────────────────────────────────────────────

struct PeerState {
    jitter: JitterBuffer,
    decoder: Decoder,
}

fn run_playback(
    dev_name: Option<String>,
    sock: Arc<UdpSocket>,
    call_id: u64,
    stop: Arc<AtomicBool>,
    out_muted: Arc<AtomicBool>,
    volume: Arc<AtomicU32>,
    pkts_recv: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
) -> Result<()> {
    let device = devices::open_output(dev_name.as_deref())
        .ok_or_else(|| anyhow::anyhow!("нет доступного устройства вывода"))?;
    let dc = device.default_output_config()?;
    let out_rate = dc.sample_rate().0;
    let out_channels = dc.channels() as usize;
    let fmt = dc.sample_format();
    let config: cpal::StreamConfig = dc.into();
    info!("Вывод: {} Hz, {} ch, {:?}", out_rate, out_channels, fmt);

    // Кольцевой буфер микса (уже в частоте устройства), 2 секунды
    let ring = HeapRb::<f32>::new(out_rate as usize * 2);
    let (mut prod, mut cons) = ring.split();

    let err_fn = |e: cpal::StreamError| error!("cpal playback: {}", e);

    // Колбэк вывода: вытаскивает сэмплы микса, дублирует на все каналы,
    // применяет громкость и мьют в реальном времени. Пустое кольцо → тишина
    // и инкремент счётчика пропусков (диагностика в UI).
    macro_rules! pop_sample {
        ($cons:expr, $und:expr, $muted:expr, $vol:expr) => {
            if $muted {
                0.0
            } else {
                match $cons.pop() {
                    Some(v) => v * $vol,
                    None => {
                        $und.fetch_add(1, Ordering::Relaxed);
                        0.0
                    }
                }
            }
        };
    }

    let stream = match fmt {
        cpal::SampleFormat::F32 => {
            let und = underruns.clone();
            device.build_output_stream(
                &config,
                move |d: &mut [f32], _| {
                    let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                    let muted = out_muted.load(Ordering::Relaxed);
                    for frame in d.chunks_mut(out_channels) {
                        let s = pop_sample!(cons, und, muted, vol);
                        for ch in frame.iter_mut() {
                            *ch = s;
                        }
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let und = underruns.clone();
            device.build_output_stream(
                &config,
                move |d: &mut [i16], _| {
                    let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                    let muted = out_muted.load(Ordering::Relaxed);
                    for frame in d.chunks_mut(out_channels) {
                        let s = pop_sample!(cons, und, muted, vol);
                        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
                        for ch in frame.iter_mut() {
                            *ch = v;
                        }
                    }
                },
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let und = underruns.clone();
            device.build_output_stream(
                &config,
                move |d: &mut [u16], _| {
                    let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                    let muted = out_muted.load(Ordering::Relaxed);
                    for frame in d.chunks_mut(out_channels) {
                        let s = pop_sample!(cons, und, muted, vol);
                        let v = ((s.clamp(-1.0, 1.0) * 32767.0) + 32768.0) as u16;
                        for ch in frame.iter_mut() {
                            *ch = v;
                        }
                    }
                },
                err_fn,
                None,
            )?
        }
        other => anyhow::bail!("формат вывода {:?} не поддерживается", other),
    };
    stream.play()?;

    sock.set_read_timeout(Some(Duration::from_millis(3)))?;

    // Стартовая подушка: пока джиттер-буфер набирает глубину, кольцо не пустое
    for _ in 0..(out_rate as usize / 1000 * RING_PREROLL_MS) {
        let _ = prod.push(0.0);
    }

    let mut peers: HashMap<u32, PeerState> = HashMap::new();
    let mut conv_out = RateConverter::new(SAMPLE_RATE, out_rate);
    let mut sbuf = vec![0u8; 65535];
    let media_key = cheburgram_protocol::derive_media_key(call_id);

    // Монотонный планировщик: фрейм ровно каждые 20 мс по нарастающему дедлайну.
    // (В v3.0 тик считался «elapsed >= 20 мс → сброс в now»: реальный период
    // выходил 22–25 мс, кольцо хронически опустошалось — те самые скрипы.)
    let mut next_pop = Instant::now() + POP_INTERVAL;

    while !stop.load(Ordering::Relaxed) {
        // приём пакетов
        match sock.recv_from(&mut sbuf) {
            Ok((n, _)) => {
                if let Some(pkt) = MediaPacket::decode_encrypted(&sbuf[..n], &media_key) {
                    if pkt.call_id == call_id && !pkt.is_keepalive && !pkt.payload.is_empty() {
                        let peer = peers.entry(pkt.sender_id).or_insert_with(|| PeerState {
                            jitter: JitterBuffer::default(),
                            decoder: make_decoder()
                                .expect("создание Opus декодера не должно падать"),
                        });
                        peer.jitter.push(pkt.seq, pkt.payload);
                        pkts_recv.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                warn!("UDP recv: {}", e);
                thread::sleep(Duration::from_millis(10));
            }
        }

        // каждые 20 мс — вытащить по фрейму на собеседника, декодировать и смикшировать
        let now = Instant::now();
        if now >= next_pop {
            // сильно отстали (засыпание ПК и т.п.) — не догоняем, а сбрасываем дедлайн
            if now.duration_since(next_pop) > POP_INTERVAL * 4 {
                next_pop = now;
            }
            next_pop += POP_INTERVAL;

            let mut mix = vec![0.0f32; FRAME_SIZE];
            let mut active = false;
            for peer in peers.values_mut() {
                let mut pcm = vec![0.0f32; FRAME_SIZE];
                let decoded = match peer.jitter.pop() {
                    Pop::Packet(data) => peer
                        .decoder
                        .decode_float(Some(data.as_slice()), pcm.as_mut_slice(), false)
                        .unwrap_or(0),
                    Pop::Fec(next) => peer
                        .decoder
                        .decode_float(Some(next.as_slice()), pcm.as_mut_slice(), true)
                        .unwrap_or(0),
                    Pop::Plc => peer
                        .decoder
                        .decode_float(None::<&[u8]>, pcm.as_mut_slice(), false)
                        .unwrap_or(0),
                };
                if decoded > 0 {
                    active = true;
                    for (m, s) in mix.iter_mut().zip(pcm.iter()) {
                        *m += *s;
                    }
                }
            }
            if active || !peers.is_empty() {
                // мягкое ограничение уровня при нескольких голосах
                for s in mix.iter_mut() {
                    *s = s.clamp(-1.5, 1.5);
                    *s = *s / (1.0 + 0.3 * s.abs()); // soft-knee
                }
                for s in conv_out.push(&mix) {
                    let _ = prod.push(s); // кольцо полное — выбрасываем
                }
            }
        }
    }
    Ok(())
}

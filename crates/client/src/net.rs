//! Сетевой слой клиента: TCP-сигналинг с автопереподключением.
//!
//! Ключевые свойства (исправления v2):
//! - собственный stop-флаг, НЕ связанный с аудио — конец звонка не убивает сеть
//! - reconnect с экспоненциальным backoff (1с → 30с) при любом обрыве
//! - heartbeat Ping каждые 15 с (сервер ждёт 60 с)
//! - UI общается через каналы: outbox (команды) / events (события)

use cheburgram_protocol::{
    read_frame_sync, write_frame_sync, ControlMessage, PROTOCOL_VERSION,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, ClientConnection};
use sha2::{Digest, Sha256};
use std::io;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
const PING_INTERVAL: Duration = Duration::from_secs(15);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// События сетевого слоя для UI
#[derive(Debug)]
pub enum NetEvent {
    /// TCP установлено, Hello+Register отправлены (ждём Registered)
    LinkUp,
    /// Соединение потеряно или не удалось; супервизор уже переподключается
    LinkDown,
    /// Сообщение от сервера
    Msg(ControlMessage),
    /// Обнаружен TLS fingerprint сервера при первом подключении (TOFU)
    TlsFingerprintDiscovered(String),
}

pub struct NetHandle {
    pub events_rx: Receiver<NetEvent>,
    pub outbox: Sender<ControlMessage>,
    pub stop: Arc<AtomicBool>,
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub struct NetCredentials {
    pub server_addr: String,
    pub client_id: String,
    pub user_code: String,
    pub display_name: String,
    pub auth_token: Option<String>,
    pub server_fingerprint: Option<String>,
    pub tls_enabled: bool,
}

#[derive(Debug)]
pub struct FingerprintVerifier {
    pub _pinned_fingerprint: Option<String>,
    pub discovered_fingerprint: Arc<Mutex<Option<String>>>,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let mut hasher = Sha256::new();
        hasher.update(end_entity.as_ref());
        let raw_fp = hasher.finalize();
        let fp_hex: String = raw_fp.iter().map(|b| format!("{:02x}", b)).collect();

        if let Ok(mut lock) = self.discovered_fingerprint.lock() {
            *lock = Some(fp_hex);
        }

        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
        ]
    }
}

pub struct TlsStreamWrapper {
    conn: Arc<Mutex<ClientConnection>>,
    sock: TcpStream,
}

impl io::Read for TlsStreamWrapper {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut conn = match self.conn.lock() {
                Ok(g) => g,
                Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e.to_string())),
            };
            let mut sock = self.sock.try_clone()?;
            let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
            let mut stream = rustls::Stream::new(&mut *conn, &mut sock);
            match stream.read(buf) {
                Ok(n) => return Ok(n),
                Err(ref e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    drop(conn);
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl io::Write for TlsStreamWrapper {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let mut sock = self.sock.try_clone()?;
        let mut stream = rustls::Stream::new(&mut *conn, &mut sock);
        stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let mut sock = self.sock.try_clone()?;
        let mut stream = rustls::Stream::new(&mut *conn, &mut sock);
        stream.flush()
    }
}

/// Запуск супервизора соединения. Живёт до stop=true.
pub fn start(creds: NetCredentials) -> NetHandle {
    let (events_tx, events_rx) = channel::<NetEvent>();
    let (outbox_tx, outbox_rx) = channel::<ControlMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    {
        let stop = stop.clone();
        let events = events_tx.clone();
        thread::spawn(move || supervisor(creds, outbox_rx, events, stop));
    }

    NetHandle {
        events_rx,
        outbox: outbox_tx,
        stop,
    }
}

fn supervisor(
    creds: NetCredentials,
    outbox: Receiver<ControlMessage>,
    events: Sender<NetEvent>,
    stop: Arc<AtomicBool>,
) {
    let mut backoff = BACKOFF_MIN;
    let _ = rustls::crypto::ring::default_provider().install_default();

    while !stop.load(Ordering::SeqCst) {
        let addr = match creds.server_addr.parse() {
            Ok(a) => Some(a),
            Err(_) => std::net::ToSocketAddrs::to_socket_addrs(&creds.server_addr.as_str())
                .ok()
                .and_then(|mut it| it.next()),
        };
        let Some(addr) = addr else {
            warn!("Не резолвится адрес сервера: {}", creds.server_addr);
            interruptible_sleep(backoff, &stop);
            backoff = (backoff * 2).min(BACKOFF_MAX);
            continue;
        };

        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                backoff = BACKOFF_MIN;

                if creds.tls_enabled {
                    let server_name = ServerName::try_from("cheburgram.local")
                        .unwrap()
                        .to_owned();

                    let discovered = Arc::new(Mutex::new(None));
                    let verifier = Arc::new(FingerprintVerifier {
                        _pinned_fingerprint: creds.server_fingerprint.clone(),
                        discovered_fingerprint: discovered.clone(),
                    });

                    let client_config = ClientConfig::builder()
                        .dangerous()
                        .with_custom_certificate_verifier(verifier)
                        .with_no_client_auth();

                    let conn = match ClientConnection::new(Arc::new(client_config), server_name) {
                        Ok(c) => Arc::new(Mutex::new(c)),
                        Err(e) => {
                            warn!("Ошибка инициализации TLS: {}", e);
                            let _ = events.send(NetEvent::LinkDown);
                            interruptible_sleep(backoff, &stop);
                            backoff = (backoff * 2).min(BACKOFF_MAX);
                            continue;
                        }
                    };

                    let read_wrapper = TlsStreamWrapper {
                        conn: conn.clone(),
                        sock: stream.try_clone().unwrap(),
                    };
                    let write_wrapper = TlsStreamWrapper {
                        conn: conn.clone(),
                        sock: stream,
                    };

                    {
                        let mut conn_guard = conn.lock().unwrap();
                        let mut raw_sock = read_wrapper.sock.try_clone().unwrap();
                        if let Err(e) = conn_guard.complete_io(&mut raw_sock) {
                            warn!("TLS рукопожатие не удалось: {}", e);
                            let _ = events.send(NetEvent::LinkDown);
                            interruptible_sleep(backoff, &stop);
                            backoff = (backoff * 2).min(BACKOFF_MAX);
                            continue;
                        }
                    }

                    if let Ok(lock) = discovered.lock() {
                        if let Some(fp) = lock.clone() {
                            info!("🔒 Обнаружен TLS fingerprint сервера: {}", fp);
                            let _ = events.send(NetEvent::TlsFingerprintDiscovered(fp));
                        }
                    }

                    info!("🔒 TLS подключено к {}", creds.server_addr);
                    run_session_io(read_wrapper, write_wrapper, &creds, &outbox, &events, &stop);
                } else {
                    info!("TCP подключено к {}", creds.server_addr);
                    let writer = stream.try_clone().unwrap();
                    run_session_io(stream, writer, &creds, &outbox, &events, &stop);
                }

                let _ = events.send(NetEvent::LinkDown);
                if !stop.load(Ordering::SeqCst) {
                    info!("Сессия завершена, переподключение через {:?}", backoff);
                }
            }
            Err(e) => {
                warn!("Подключение не удалось: {}", e);
                let _ = events.send(NetEvent::LinkDown);
            }
        }
        interruptible_sleep(backoff, &stop);
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn run_session_io<R, W>(
    reader_stream: R,
    mut writer: W,
    creds: &NetCredentials,
    outbox: &Receiver<ControlMessage>,
    events: &Sender<NetEvent>,
    stop: &Arc<AtomicBool>,
) where
    R: io::Read + Send + 'static,
    W: io::Write + Send + 'static,
{
    let mut sync_reader = io::BufReader::new(reader_stream);

    // рукопожатие: Hello
    if write_frame_sync(&mut writer, &ControlMessage::Hello { protocol_version: PROTOCOL_VERSION })
        .is_err()
    {
        return;
    }

    // получение Challenge от сервера
    let challenge_msg = match read_frame_sync(&mut sync_reader) {
        Ok(m) => m,
        Err(e) => {
            warn!("Ошибка чтения Challenge: {}", e);
            return;
        }
    };

    let nonce = match challenge_msg {
        ControlMessage::Challenge { nonce } => nonce,
        other => {
            let _ = events.send(NetEvent::Msg(other));
            return;
        }
    };

    // отправка Auth (с HMAC-доказательством) или Register
    let init_msg = if let Some(token) = &creds.auth_token {
        let token_hash = cheburgram_protocol::compute_token_hash(token);
        let proof = cheburgram_protocol::compute_auth_proof(&token_hash, &nonce);
        ControlMessage::Auth {
            user_code: creds.user_code.clone(),
            proof,
        }
    } else {
        ControlMessage::Register {
            client_id: creds.client_id.clone(),
            user_code: creds.user_code.clone(),
            name: creds.display_name.clone(),
        }
    };
    if write_frame_sync(&mut writer, &init_msg).is_err() {
        return;
    }

    // поток чтения
    let reader_alive = Arc::new(AtomicBool::new(true));
    {
        let reader_alive = reader_alive.clone();
        let events = events.clone();
        thread::spawn(move || {
            let mut reader = sync_reader;
            loop {
                match read_frame_sync(&mut reader) {
                    Ok(msg) => {
                        if events.send(NetEvent::Msg(msg)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            reader_alive.store(false, Ordering::SeqCst);
        });
    }

    let _ = events.send(NetEvent::LinkUp);

    // цикл записи: outbox + heartbeat
    let mut last_ping = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) || !reader_alive.load(Ordering::Relaxed) {
            break;
        }
        while let Ok(msg) = outbox.try_recv() {
            if write_frame_sync(&mut writer, &msg).is_err() {
                return;
            }
        }
        if last_ping.elapsed() >= PING_INTERVAL {
            last_ping = Instant::now();
            if write_frame_sync(&mut writer, &ControlMessage::Ping).is_err() {
                return;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Сон с проверкой stop каждые 100 мс
fn interruptible_sleep(d: Duration, stop: &Arc<AtomicBool>) {
    let started = Instant::now();
    while started.elapsed() < d {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

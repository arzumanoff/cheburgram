//! E2E: два клиента через живой сервер (TCP + UDP релей).
//! Сценарий: регистрация → статусы → звонок → медиа-релей → завершение.

use cheburgram_protocol::{
    read_frame_sync, write_frame_sync, AuthError, AuthOutcome, ControlMessage, MediaPacket,
    PROTOCOL_VERSION,
};
use cheburgram_server::{handle_client, route_media_packet, ClientRegistry, SharedState, State};
use std::io::BufReader;
use std::net::{TcpStream, UdpSocket};
use std::sync::Arc;
use tokio::net::{TcpListener, UdpSocket as TokioUdpSocket};
use tokio::sync::Mutex;

struct TestServer {
    tcp_port: u16,
    udp_port: u16,
}

async fn start_server() -> TestServer {
    let state: SharedState = Arc::new(Mutex::new(State::default()));
    let registry = Arc::new(Mutex::new(ClientRegistry::default()));

    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp_port = tcp.local_addr().unwrap().port();

    let udp = Arc::new(TokioUdpSocket::bind("127.0.0.1:0").await.unwrap());
    let udp_port = udp.local_addr().unwrap().port();

    {
        let state = state.clone();
        let registry = registry.clone();
        tokio::spawn(async move {
            loop {
                let (stream, peer_addr) = tcp.accept().await.unwrap();
                let s = state.clone();
                let r = registry.clone();
                let ip = peer_addr.ip();
                tokio::spawn(async move {
                    let _ = handle_client(stream, ip, s, r).await;
                });
            }
        });
    }

    {
        let state = state.clone();
        let udp_recv = udp.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                if let Ok((len, src)) = udp_recv.recv_from(&mut buf).await {
                    let route = {
                        let mut st = state.lock().await;
                        route_media_packet(&mut st, src, &buf[..len])
                    };
                    if let Some((dst, bytes)) = route {
                        let _ = udp_recv.send_to(&bytes, dst).await;
                    }
                }
            }
        });
    }

    TestServer { tcp_port, udp_port }
}

struct TestClient {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    udp: UdpSocket,
}

impl TestClient {
    fn connect(tcp_port: u16, client_id: &str, name: &str) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", tcp_port)).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut c = TestClient { stream, reader, udp };
        c.send(&ControlMessage::Hello { protocol_version: PROTOCOL_VERSION });
        let _nonce = match c.recv() {
            ControlMessage::Challenge { nonce } => nonce,
            m => panic!("Ожидался Challenge, получено: {:?}", m),
        };
        c.send(&ControlMessage::Register {
            client_id: client_id.into(),
            user_code: String::new(),
            name: name.into(),
        });
        c
    }

    fn connect_auth(tcp_port: u16, user_code: &str, token: &str) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", tcp_port)).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut c = TestClient { stream, reader, udp };
        c.send(&ControlMessage::Hello { protocol_version: PROTOCOL_VERSION });
        let nonce = match c.recv() {
            ControlMessage::Challenge { nonce } => nonce,
            m => panic!("Ожидался Challenge, получено: {:?}", m),
        };
        let token_hash = cheburgram_protocol::compute_token_hash(token);
        let proof = cheburgram_protocol::compute_auth_proof(&token_hash, &nonce);
        c.send(&ControlMessage::Auth {
            user_code: user_code.into(),
            proof,
        });
        c
    }

    fn send(&mut self, msg: &ControlMessage) {
        write_frame_sync(&mut self.stream, msg).unwrap();
    }

    fn recv(&mut self) -> ControlMessage {
        read_frame_sync(&mut self.reader).unwrap()
    }

    fn wait_for(&mut self, pred: impl Fn(&ControlMessage) -> bool) -> ControlMessage {
        for _ in 0..50 {
            let msg = self.recv();
            if pred(&msg) {
                return msg;
            }
        }
        panic!("не дождались нужного сообщения за 50 кадров");
    }
}

#[test]
fn e2e_media_relay_full() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(start_server());
    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut a = TestClient::connect(server.tcp_port, "client-A2", "Алиса");
    let mut b = TestClient::connect(server.tcp_port, "client-B2", "Борис");

    let (code_a, peer_a) = match a.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } })) {
        ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { user_code, peer_id, .. } } => (user_code, peer_id),
        _ => unreachable!(),
    };
    let (code_b, peer_b) = match b.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } })) {
        ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { user_code, peer_id, .. } } => (user_code, peer_id),
        _ => unreachable!(),
    };
    assert_ne!(code_a, code_b);

    // ── статусы: A видит B онлайн ──
    a.send(&ControlMessage::GetFriendsStatus { user_codes: vec![code_b.clone()] });
    match a.wait_for(|m| matches!(m, ControlMessage::FriendsStatus { .. })) {
        ControlMessage::FriendsStatus { friends } => {
            assert_eq!(friends.len(), 1);
            assert!(friends[0].is_online);
        }
        _ => unreachable!(),
    }

    // ── чат: A -> B ──
    a.send(&ControlMessage::SendTextMessage {
        target_code: code_b.clone(),
        text: "Привет!".into(),
        message_id: "msg-1".into(),
    });
    match b.wait_for(|m| matches!(m, ControlMessage::IncomingTextMessage { .. })) {
        ControlMessage::IncomingTextMessage { msg } => {
            assert_eq!(msg.text, "Привет!");
            assert_eq!(msg.from_code, code_a);
        }
        _ => unreachable!(),
    }

    // ── звонок: A звонит B, B принимает ──
    a.send(&ControlMessage::CallRequest { target_code: code_b.clone() });
    match b.wait_for(|m| matches!(m, ControlMessage::IncomingCall { .. })) {
        ControlMessage::IncomingCall { from_peer_id, from_name, .. } => {
            assert_eq!(from_peer_id, peer_a);
            assert_eq!(from_name, "Алиса");
            b.send(&ControlMessage::CallAccept { target_peer_id: from_peer_id });
        }
        _ => unreachable!(),
    }
    let call_id = match a.wait_for(|m| matches!(m, ControlMessage::CallAccepted { .. })) {
        ControlMessage::CallAccepted { call_id, peer_id, peer_name } => {
            assert_eq!(peer_id, peer_b);
            assert_eq!(peer_name, "Борис");
            call_id
        }
        _ => unreachable!(),
    };
    let call_id_b = match b.wait_for(|m| matches!(m, ControlMessage::CallAccepted { .. })) {
        ControlMessage::CallAccepted { call_id, .. } => call_id,
        _ => unreachable!(),
    };
    assert_eq!(call_id, call_id_b, "оба клиента должны получить один call_id");

    // ── keepalive от обоих: регистрация UDP-адресов на релее ──
    let ka_a = MediaPacket::keepalive(call_id, peer_a).encode();
    let ka_b = MediaPacket::keepalive(call_id, peer_b).encode();
    a.udp.send_to(&ka_a, ("127.0.0.1", server.udp_port)).unwrap();
    b.udp.send_to(&ka_b, ("127.0.0.1", server.udp_port)).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));

    // ── медиапакет A → релей → B ──
    let media_key = cheburgram_protocol::derive_media_key(call_id);
    let audio_frame = vec![0xAB; 120];
    let pkt = MediaPacket::new(call_id, peer_a, 1, audio_frame.clone()).encode_encrypted(&media_key);
    a.udp.send_to(&pkt, ("127.0.0.1", server.udp_port)).unwrap();

    let mut buf = [0u8; 2048];
    let (n, _) = b.udp.recv_from(&mut buf).expect("B не получил медиапакет через релей");
    let received = MediaPacket::decode_encrypted(&buf[..n], &media_key).unwrap();
    assert_eq!(received.seq, 1);
    assert_eq!(received.sender_id, peer_a);
    assert_eq!(received.payload, audio_frame);

    // и в обратную сторону
    let pkt_b = MediaPacket::new(call_id, peer_b, 1, vec![0xCD; 80]).encode_encrypted(&media_key);
    b.udp.send_to(&pkt_b, ("127.0.0.1", server.udp_port)).unwrap();
    let (n, _) = a.udp.recv_from(&mut buf).expect("A не получил ответный медиапакет");
    let received = MediaPacket::decode_encrypted(&buf[..n], &media_key).unwrap();
    assert_eq!(received.sender_id, peer_b);

    // ── завершение: B получает CallEnded, сеть A остаётся жива ──
    a.send(&ControlMessage::CallEnd);
    b.wait_for(|m| matches!(m, ControlMessage::CallEnded { .. }));

    a.send(&ControlMessage::GetFriendsStatus { user_codes: vec![code_b.clone()] });
    match a.wait_for(|m| matches!(m, ControlMessage::FriendsStatus { .. })) {
        ControlMessage::FriendsStatus { friends } => assert!(friends[0].is_online),
        _ => unreachable!(),
    }

    drop(rt);
}

#[test]
fn e2e_session_replaced() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(start_server());
    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut a1 = TestClient::connect(server.tcp_port, "client-same", "Алиса");
    let code1 = match a1.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } })) {
        ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { user_code, .. } } => user_code,
        _ => unreachable!(),
    };

    let mut a2 = TestClient::connect(server.tcp_port, "client-same", "Алиса");
    let code2 = match a2.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } })) {
        ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { user_code, .. } } => user_code,
        _ => unreachable!(),
    };
    assert_eq!(code1, code2, "тот же аккаунт — тот же ID");

    a1.wait_for(|m| matches!(m, ControlMessage::SessionReplaced));

    a2.send(&ControlMessage::GetFriendsStatus { user_codes: vec![code1.clone()] });
    match a2.wait_for(|m| matches!(m, ControlMessage::FriendsStatus { .. })) {
        ControlMessage::FriendsStatus { friends } => {
            assert!(friends[0].is_online, "новая сессия должна быть онлайн");
        }
        _ => unreachable!(),
    }
    drop(rt);
}

#[test]
fn e2e_call_accept_validation() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(start_server());
    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut a = TestClient::connect(server.tcp_port, "client-A3", "Алиса");
    let mut b = TestClient::connect(server.tcp_port, "client-B3", "Борис");
    let _ = a.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } }));
    let _ = b.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } }));

    b.send(&ControlMessage::CallAccept { target_peer_id: 1 });
    match b.wait_for(|m| matches!(m, ControlMessage::Error { .. })) {
        ControlMessage::Error { message } => {
            assert!(message.contains("Нет входящего"), "message={}", message);
        }
        _ => unreachable!(),
    }

    a.stream
        .set_read_timeout(Some(std::time::Duration::from_millis(300)))
        .unwrap();
    let mut got_accepted = false;
    for _ in 0..5 {
        if let Ok(msg) = read_frame_sync(&mut a.reader) {
            if matches!(msg, ControlMessage::CallAccepted { .. }) {
                got_accepted = true;
            }
        } else {
            break;
        }
    }
    assert!(!got_accepted, "фиктивный CallAccept не должен создавать звонок");
    drop(rt);
}

#[test]
fn e2e_offline_message_delivery() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(start_server());
    std::thread::sleep(std::time::Duration::from_millis(100));

    let code_b;
    {
        let mut b = TestClient::connect(server.tcp_port, "client-B4", "Борис");
        code_b = match b.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } })) {
            ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { user_code, .. } } => user_code,
            _ => unreachable!(),
        };
    }
    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut a = TestClient::connect(server.tcp_port, "client-A4", "Алиса");
    let _ = a.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } }));
    a.send(&ControlMessage::SendTextMessage {
        target_code: code_b.clone(),
        text: "Пока ты спал".into(),
        message_id: "offline-1".into(),
    });
    match a.wait_for(|m| matches!(m, ControlMessage::TextMessageAck { .. })) {
        ControlMessage::TextMessageAck { delivered, .. } => assert!(!delivered),
        _ => unreachable!(),
    }

    let mut b2 = TestClient::connect(server.tcp_port, "client-B4", "Борис");
    match b2.wait_for(|m| matches!(m, ControlMessage::PendingTextMessages { .. })) {
        ControlMessage::PendingTextMessages { messages } => {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].text, "Пока ты спал");
        }
        _ => unreachable!(),
    }
    drop(rt);
}

#[test]
fn e2e_auth_token_flow() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let server = rt.block_on(start_server());
    std::thread::sleep(std::time::Duration::from_millis(100));

    // 1. Регистрация первого клиента, получение auth_token
    let mut client1 = TestClient::connect(server.tcp_port, "client-auth-1", "Иван");
    let (code, token) = match client1.wait_for(|m| matches!(m, ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } })) {
        ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { user_code, auth_token, .. } } => (user_code, auth_token.unwrap()),
        _ => unreachable!(),
    };
    assert!(!token.is_empty());
    drop(client1);

    std::thread::sleep(std::time::Duration::from_millis(100));

    // 2. Вход с верным HMAC-доказательством
    let mut client2 = TestClient::connect_auth(server.tcp_port, &code, &token);
    match client2.wait_for(|m| matches!(m, ControlMessage::AuthResponse { .. })) {
        ControlMessage::AuthResponse { outcome: AuthOutcome::Ok { .. } } => {}
        _ => panic!("Авторизация должна пройти"),
    }
    drop(client2);

    std::thread::sleep(std::time::Duration::from_millis(100));

    // 3. Попытка входа с неверным токеном
    let mut client3 = TestClient::connect_auth(server.tcp_port, &code, "wrong_token_123");
    match client3.wait_for(|m| matches!(m, ControlMessage::AuthResponse { .. })) {
        ControlMessage::AuthResponse { outcome: AuthOutcome::Failed(err) } => {
            assert_eq!(err, AuthError::InvalidToken);
        }
        _ => panic!("Авторизация с неверным токеном должна отклоняться"),
    }
    drop(rt);
}

#[test]
fn test_legacy_plaintext_notification() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let listener = rt.block_on(async { TcpListener::bind("127.0.0.1:0").await.unwrap() });
    let port = listener.local_addr().unwrap().port();

    rt.spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _ = cheburgram_server::handle_legacy_plaintext_client(stream).await;
    });

    std::thread::sleep(std::time::Duration::from_millis(100));

    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write_frame_sync(&mut stream, &ControlMessage::Hello { protocol_version: 2 }).unwrap();

    let mut reader = BufReader::new(stream);
    let reply = read_frame_sync(&mut reader).unwrap();

    assert!(matches!(
        reply,
        ControlMessage::VersionMismatch { min: 3, max: 3 }
    ));
}

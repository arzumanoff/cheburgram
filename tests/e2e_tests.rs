use cheburgram_protocol::{AudioPacket, ControlMessage, TextMessage};
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

fn send_msg(stream: &mut TcpStream, msg: &ControlMessage) {
    let json = serde_json::to_string(msg).unwrap();
    stream.write_all(format!("{}\n", json).as_bytes()).unwrap();
    stream.flush().unwrap();
}

fn read_msg(reader: &mut BufReader<TcpStream>) -> ControlMessage {
    let mut line = String::new();
    reader.read_line(&mut line).expect("Failed to read line");
    serde_json::from_str(&line).expect(&format!("Failed to parse line: {}", line))
}

#[test]
fn test_full_messenger_flow_e2e() {
    // 1. Запускаем сервер на случайном порту в фоновом потоке
    let tcp_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_port = tcp_listener.local_addr().unwrap().port();
    let udp_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let udp_port = udp_socket.local_addr().unwrap().port();

    let server_addr_str = format!("127.0.0.1:{}", tcp_port);

    // Запускаем сервер во время теста
    let registry = Arc::new(Mutex::new(cheburgram_server::ClientRegistry::default()));
    let state: cheburgram_server::SharedState = Arc::new(tokio::sync::Mutex::new(cheburgram_server::State::default()));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    let reg_c = registry.clone();
    let st_c = state.clone();
    let udp_sock_c = Arc::new(tokio::net::UdpSocket::from_std(udp_socket).unwrap());

    runtime.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(tcp_listener).unwrap();
        loop {
            if let Ok((stream, _)) = listener.accept().await {
                let st = st_c.clone();
                let reg = reg_c.clone();
                tokio::spawn(async move {
                    let _ = cheburgram_server::handle_client(stream, st, reg).await;
                });
            }
        }
    });

    thread::sleep(Duration::from_millis(100));

    // 2. Клиент А (Алиса) подключается
    let mut stream_a = TcpStream::connect(&server_addr_str).unwrap();
    let mut reader_a = BufReader::new(stream_a.try_clone().unwrap());

    send_msg(&mut stream_a, &ControlMessage::Register {
        client_id: "uuid-alice".to_string(),
        user_code: "111111".to_string(),
        name: "Алиса".to_string(),
    });

    let reg_a = read_msg(&mut reader_a);
    let (peer_a, code_a) = match reg_a {
        ControlMessage::Registered { peer_id, user_code, .. } => (peer_id, user_code),
        other => panic!("Expected Registered for Alice, got {:?}", other),
    };
    assert_eq!(code_a, "111111");

    // 3. Клиент Б (Боб) подключается
    let mut stream_b = TcpStream::connect(&server_addr_str).unwrap();
    let mut reader_b = BufReader::new(stream_b.try_clone().unwrap());

    send_msg(&mut stream_b, &ControlMessage::Register {
        client_id: "uuid-bob".to_string(),
        user_code: "222222".to_string(),
        name: "Боб".to_string(),
    });

    let reg_b = read_msg(&mut reader_b);
    let (peer_b, code_b) = match reg_b {
        ControlMessage::Registered { peer_id, user_code, .. } => (peer_id, user_code),
        other => panic!("Expected Registered for Bob, got {:?}", other),
    };
    assert_eq!(code_b, "222222");

    // Читаем разглашение статуса Боба у Алисы
    let status_msg = read_msg(&mut reader_a);
    match status_msg {
        ControlMessage::UserStatusChanged { user_code, is_online, .. } => {
            assert_eq!(user_code, "222222");
            assert!(is_online);
        }
        other => panic!("Expected UserStatusChanged, got {:?}", other),
    }

    // 4. ТЕСТ 1: Запрос в друзья
    send_msg(&mut stream_a, &ControlMessage::SendFriendRequest {
        target_code: "222222".to_string(),
    });

    let req_for_b = read_msg(&mut reader_b);
    match req_for_b {
        ControlMessage::IncomingFriendRequest { from_code, from_name } => {
            assert_eq!(from_code, "111111");
            assert_eq!(from_name, "Алиса");
        }
        other => panic!("Expected IncomingFriendRequest, got {:?}", other),
    }

    let ack_for_a = read_msg(&mut reader_a); // Error notification "Запрос отправлен"
    assert!(matches!(ack_for_a, ControlMessage::Error { .. }));

    // Боб принимает запрос
    send_msg(&mut stream_b, &ControlMessage::AcceptFriendRequest {
        from_code: "111111".to_string(),
    });

    let b_friend_ack = read_msg(&mut reader_b);
    assert!(matches!(b_friend_ack, ControlMessage::FriendRequestAccepted { user_code, .. } if user_code == "111111"));

    let a_friend_ack = read_msg(&mut reader_a);
    assert!(matches!(a_friend_ack, ControlMessage::FriendRequestAccepted { user_code, .. } if user_code == "222222"));

    // 5. ТЕСТ 2: Передача SMS / Чат в реальном времени
    send_msg(&mut stream_a, &ControlMessage::SendTextMessage {
        target_code: "222222".to_string(),
        text: "Привет, Боб! Это SMS тест.".to_string(),
        message_id: "sms-1".to_string(),
    });

    let b_sms = read_msg(&mut reader_b);
    match b_sms {
        ControlMessage::IncomingTextMessage { msg } => {
            assert_eq!(msg.from_code, "111111");
            assert_eq!(msg.text, "Привет, Боб! Это SMS тест.");
            assert_eq!(msg.id, "sms-1");
        }
        other => panic!("Expected IncomingTextMessage, got {:?}", other),
    }

    let a_sms_ack = read_msg(&mut reader_a);
    match a_sms_ack {
        ControlMessage::TextMessageAck { message_id, delivered } => {
            assert_eq!(message_id, "sms-1");
            assert!(delivered);
        }
        other => panic!("Expected TextMessageAck, got {:?}", other),
    }

    // 6. ТЕСТ 3: Голосовой звонок (Call Signalling)
    send_msg(&mut stream_a, &ControlMessage::CallRequest {
        target_code: "222222".to_string(),
    });

    let b_call_inc = read_msg(&mut reader_b);
    let from_peer_id = match b_call_inc {
        ControlMessage::IncomingCall { from_code, from_name, from_peer_id } => {
            assert_eq!(from_code, "111111");
            assert_eq!(from_name, "Алиса");
            from_peer_id
        }
        other => panic!("Expected IncomingCall, got {:?}", other),
    };
    assert_eq!(from_peer_id, peer_a);

    // Боб принимает звонок
    send_msg(&mut stream_b, &ControlMessage::CallAccept {
        target_peer_id: peer_a,
    });

    let b_call_acc = read_msg(&mut reader_b);
    let call_id_b = match b_call_acc {
        ControlMessage::CallAccepted { peer_id, peer_name, call_id } => {
            assert_eq!(peer_id, peer_a);
            assert_eq!(peer_name, "Алиса");
            call_id
        }
        other => panic!("Expected CallAccepted for Bob, got {:?}", other),
    };

    let a_call_acc = read_msg(&mut reader_a);
    let call_id_a = match a_call_acc {
        ControlMessage::CallAccepted { peer_id, peer_name, call_id } => {
            assert_eq!(peer_id, peer_b);
            assert_eq!(peer_name, "Боб");
            call_id
        }
        other => panic!("Expected CallAccepted for Alice, got {:?}", other),
    };

    // ГАРАНТИРУЕМ единый call_id!
    assert_eq!(call_id_a, call_id_b);

    // 7. ТЕСТ 4: Завершение звонка
    send_msg(&mut stream_a, &ControlMessage::CallEnd);

    let b_call_ended = read_msg(&mut reader_b);
    match b_call_ended {
        ControlMessage::CallEnded { peer_name } => {
            assert_eq!(peer_name, "Алиса");
        }
        other => panic!("Expected CallEnded for Bob, got {:?}", other),
    }

    println!("🎉 E2E Тесты мессенджера (Регистрация, Друзья, SMS, Звонки) пройдены УСПЕШНО!");
}

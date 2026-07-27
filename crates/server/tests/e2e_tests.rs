use cheburgram_protocol::ControlMessage;
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};

async fn send_msg(stream: &mut tokio::net::tcp::OwnedWriteHalf, msg: &ControlMessage) {
    let json = serde_json::to_string(msg).unwrap();
    stream.write_all(format!("{}\n", json).as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
}

async fn read_msg(lines: &mut tokio::io::Lines<BufReader<tokio::net::tcp::OwnedReadHalf>>, who: &str) -> ControlMessage {
    loop {
        let line = lines.next_line().await.unwrap().expect("Unexpected EOF from server");
        let msg: ControlMessage = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("Failed to parse line for {}: '{}', error: {:?}", who, line, e));
        if matches!(msg, ControlMessage::UserStatusChanged { .. }) {
            println!("[{}] Received UserStatusChanged (ignored)", who);
            continue;
        }
        println!("[{}] Received msg: {:?}", who, msg);
        return msg;
    }
}

#[tokio::test]
async fn test_full_messenger_flow_e2e() {
    println!("Step 1: Spawning test server...");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_addr = listener.local_addr().unwrap();

    let registry = Arc::new(Mutex::new(cheburgram_server::ClientRegistry::default()));
    let state: cheburgram_server::SharedState = Arc::new(Mutex::new(cheburgram_server::State::default()));

    let reg_c = registry.clone();
    let st_c = state.clone();

    tokio::spawn(async move {
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

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // 1. Алиса регистрируется
    println!("Step 2: Connecting Alice...");
    let stream_a = TcpStream::connect(server_addr).await.unwrap();
    let (reader_a, mut writer_a) = stream_a.into_split();
    let mut lines_a = BufReader::new(reader_a).lines();

    send_msg(&mut writer_a, &ControlMessage::Register {
        client_id: "uuid-alice".to_string(),
        user_code: "111111".to_string(),
        name: "Алиса".to_string(),
    }).await;

    let reg_a = read_msg(&mut lines_a, "Alice").await;
    let (peer_a, code_a) = match reg_a {
        ControlMessage::Registered { peer_id, user_code, .. } => (peer_id, user_code),
        other => panic!("Expected Registered for Alice, got {:?}", other),
    };
    assert_eq!(code_a, "111111");

    // 2. Боб регистрируется
    println!("Step 3: Connecting Bob...");
    let stream_b = TcpStream::connect(server_addr).await.unwrap();
    let (reader_b, mut writer_b) = stream_b.into_split();
    let mut lines_b = BufReader::new(reader_b).lines();

    send_msg(&mut writer_b, &ControlMessage::Register {
        client_id: "uuid-bob".to_string(),
        user_code: "222222".to_string(),
        name: "Боб".to_string(),
    }).await;

    let reg_b = read_msg(&mut lines_b, "Bob").await;
    let (peer_b, code_b) = match reg_b {
        ControlMessage::Registered { peer_id, user_code, .. } => (peer_id, user_code),
        other => panic!("Expected Registered for Bob, got {:?}", other),
    };
    assert_eq!(code_b, "222222");

    // 3. ТЕСТ: Запрос в друзья
    println!("Step 4: Alice sending friend request to Bob...");
    send_msg(&mut writer_a, &ControlMessage::SendFriendRequest {
        target_code: "222222".to_string(),
    }).await;

    let req_for_b = read_msg(&mut lines_b, "Bob").await;
    match req_for_b {
        ControlMessage::IncomingFriendRequest { from_code, from_name } => {
            assert_eq!(from_code, "111111");
            assert_eq!(from_name, "Алиса");
        }
        other => panic!("Expected IncomingFriendRequest, got {:?}", other),
    }

    let _ack_a = read_msg(&mut lines_a, "Alice").await;

    // Боб принимает запрос
    println!("Step 5: Bob accepting friend request...");
    send_msg(&mut writer_b, &ControlMessage::AcceptFriendRequest {
        from_code: "111111".to_string(),
    }).await;

    let b_friend_ack = read_msg(&mut lines_b, "Bob").await;
    assert!(matches!(b_friend_ack, ControlMessage::FriendRequestAccepted { user_code, .. } if user_code == "111111"));

    let a_friend_ack = read_msg(&mut lines_a, "Alice").await;
    assert!(matches!(a_friend_ack, ControlMessage::FriendRequestAccepted { user_code, .. } if user_code == "222222"));

    // 4. ТЕСТ: Текстовые сообщения (SMS / Чат)
    println!("Step 6: Alice sending Text Message (SMS) to Bob...");
    send_msg(&mut writer_a, &ControlMessage::SendTextMessage {
        target_code: "222222".to_string(),
        text: "Привет Боб! Проверка SMS.".to_string(),
        message_id: "sms-100".to_string(),
    }).await;

    let b_sms = read_msg(&mut lines_b, "Bob").await;
    match b_sms {
        ControlMessage::IncomingTextMessage { msg } => {
            assert_eq!(msg.from_code, "111111");
            assert_eq!(msg.text, "Привет Боб! Проверка SMS.");
            assert_eq!(msg.id, "sms-100");
        }
        other => panic!("Expected IncomingTextMessage for Bob, got {:?}", other),
    }

    let a_sms_ack = read_msg(&mut lines_a, "Alice").await;
    match a_sms_ack {
        ControlMessage::TextMessageAck { message_id, delivered } => {
            assert_eq!(message_id, "sms-100");
            assert!(delivered);
        }
        other => panic!("Expected TextMessageAck for Alice, got {:?}", other),
    }

    // 5. ТЕСТ: Голосовой звонок (Call Signalling & call_id sync)
    println!("Step 7: Alice calling Bob...");
    send_msg(&mut writer_a, &ControlMessage::CallRequest {
        target_code: "222222".to_string(),
    }).await;

    let b_call_inc = read_msg(&mut lines_b, "Bob").await;
    let from_peer_id = match b_call_inc {
        ControlMessage::IncomingCall { from_code, from_name, from_peer_id } => {
            assert_eq!(from_code, "111111");
            assert_eq!(from_name, "Алиса");
            from_peer_id
        }
        other => panic!("Expected IncomingCall for Bob, got {:?}", other),
    };
    assert_eq!(from_peer_id, peer_a);

    println!("Step 8: Bob accepting call...");
    send_msg(&mut writer_b, &ControlMessage::CallAccept {
        target_peer_id: peer_a,
    }).await;

    let b_call_acc = read_msg(&mut lines_b, "Bob").await;
    let call_id_b = match b_call_acc {
        ControlMessage::CallAccepted { peer_id, peer_name, call_id } => {
            assert_eq!(peer_id, peer_a);
            assert_eq!(peer_name, "Алиса");
            call_id
        }
        other => panic!("Expected CallAccepted for Bob, got {:?}", other),
    };

    let a_call_acc = read_msg(&mut lines_a, "Alice").await;
    let call_id_a = match a_call_acc {
        ControlMessage::CallAccepted { peer_id, peer_name, call_id } => {
            assert_eq!(peer_id, peer_b);
            assert_eq!(peer_name, "Боб");
            call_id
        }
        other => panic!("Expected CallAccepted for Alice, got {:?}", other),
    };

    assert_eq!(call_id_a, call_id_b);

    println!("Step 9: Alice ending call...");
    send_msg(&mut writer_a, &ControlMessage::CallEnd).await;

    let b_call_ended = read_msg(&mut lines_b, "Bob").await;
    match b_call_ended {
        ControlMessage::CallEnded { peer_name } => {
            assert_eq!(peer_name, "Алиса");
        }
        other => panic!("Expected CallEnded for Bob, got {:?}", other),
    }

    // 6. ТЕСТ: Офлайн SMS сообщения
    println!("Step 10: Testing Offline SMS delivery...");
    // Боб отключается
    drop(writer_b);
    drop(lines_b);
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // Алиса шлёт SMS Бобу пока он офлайн
    send_msg(&mut writer_a, &ControlMessage::SendTextMessage {
        target_code: "222222".to_string(),
        text: "Сообщение пока ты офлайн!".to_string(),
        message_id: "sms-offline-1".to_string(),
    }).await;

    let a_offline_ack = read_msg(&mut lines_a, "Alice").await;
    match a_offline_ack {
        ControlMessage::TextMessageAck { message_id, delivered } => {
            assert_eq!(message_id, "sms-offline-1");
            assert!(!delivered); // delivered = false означает сохранено в офлайн очередь!
        }
        other => panic!("Expected TextMessageAck for offline msg, got {:?}", other),
    }

    // Боб снова заходит онлайн
    let stream_b_reconnect = TcpStream::connect(server_addr).await.unwrap();
    let (reader_b2, mut writer_b2) = stream_b_reconnect.into_split();
    let mut lines_b2 = BufReader::new(reader_b2).lines();

    send_msg(&mut writer_b2, &ControlMessage::Register {
        client_id: "uuid-bob".to_string(),
        user_code: "222222".to_string(),
        name: "Боб".to_string(),
    }).await;

    let reg_b2 = read_msg(&mut lines_b2, "Bob").await;
    assert!(matches!(reg_b2, ControlMessage::Registered { .. }));

    // Боб получает отложенные сообщения
    let pending_b = read_msg(&mut lines_b2, "Bob").await;
    match pending_b {
        ControlMessage::PendingTextMessages { messages } => {
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].text, "Сообщение пока ты офлайн!");
            assert_eq!(messages[0].from_code, "111111");
        }
        other => panic!("Expected PendingTextMessages for Bob, got {:?}", other),
    }

    println!("🎉 Все E2E Тесты (Регистрация, Друзья, SMS в реальном времени, Офлайн SMS, Звонки) пройдены 100% УСПЕШНО!");
}

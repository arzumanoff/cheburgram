use cheburgram_protocol::ControlMessage;
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};

#[tokio::test]
async fn live_network_socket_test() {
    println!("\n========================================================");
    println!("🧪 ЗАПУСК ПРЯМОГО СЕТЕВОГО ТЕСТА СОКЕТОВ (REAL TOKIO SOCKET TEST)");
    println!("========================================================\n");

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("Не удалось привязать сокет");
    let server_addr = listener.local_addr().unwrap();
    println!("[СЕРВЕР] Запущен реальный TCP сервер на сокете {}", server_addr);

    let registry = Arc::new(Mutex::new(cheburgram_server::ClientRegistry::default()));
    let state: cheburgram_server::SharedState = Arc::new(Mutex::new(cheburgram_server::State::default()));

    let reg_c = registry.clone();
    let st_c = state.clone();

    tokio::spawn(async move {
        loop {
            if let Ok((stream, addr)) = listener.accept().await {
                println!("[СЕРВЕР] Принято подключение сокета: {}", addr);
                let st = st_c.clone();
                let reg = reg_c.clone();
                tokio::spawn(async move {
                    let _ = cheburgram_server::handle_client(stream, st, reg).await;
                });
            }
        }
    });

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    // 1. Клиент Алиса подключается к серверу по сетевому сокету
    println!("\n[КЛИЕНТ 1 (АЛИСА)] Открываем сетевой сокет TcpStream::connect({})...", server_addr);
    let client_a = TcpStream::connect(server_addr).await.unwrap();
    client_a.set_nodelay(true).unwrap();
    let (reader_a, mut writer_a) = client_a.into_split();
    let mut lines_a = BufReader::new(reader_a).lines();

    let reg_msg_a = ControlMessage::Register {
        client_id: "uuid-alice".to_string(),
        user_code: "111111".to_string(),
        name: "Алиса".to_string(),
    };
    let json_a = serde_json::to_string(&reg_msg_a).unwrap();
    writer_a.write_all(format!("{}\n", json_a).as_bytes()).await.unwrap();
    writer_a.flush().await.unwrap();
    println!("[КЛИЕНТ 1 (АЛИСА)] Отправлен пакет регистрации Register ('Алиса', ID: 111111) с принудительным flush()");

    let resp_line_a = lines_a.next_line().await.unwrap().expect("Нет ответа от сервера");
    println!("[КЛИЕНТ 1 (АЛИСА)] 📥 Ответ от сервера: {}", resp_line_a);
    let resp_a: ControlMessage = serde_json::from_str(&resp_line_a).unwrap();
    assert!(matches!(resp_a, ControlMessage::Registered { user_code, .. } if user_code == "111111"));

    // 2. Клиент Боб подключается к серверу по сетевому сокету
    println!("\n[КЛИЕНТ 2 (БОБ)] Открываем сетевой сокет TcpStream::connect({})...", server_addr);
    let client_b = TcpStream::connect(server_addr).await.unwrap();
    client_b.set_nodelay(true).unwrap();
    let (reader_b, mut writer_b) = client_b.into_split();
    let mut lines_b = BufReader::new(reader_b).lines();

    let reg_msg_b = ControlMessage::Register {
        client_id: "uuid-bob".to_string(),
        user_code: "222222".to_string(),
        name: "Боб".to_string(),
    };
    let json_b = serde_json::to_string(&reg_msg_b).unwrap();
    writer_b.write_all(format!("{}\n", json_b).as_bytes()).await.unwrap();
    writer_b.flush().await.unwrap();
    println!("[КЛИЕНТ 2 (БОБ)] Отправлен пакет регистрации Register ('Боб', ID: 222222) с принудительным flush()");

    let resp_line_b = lines_b.next_line().await.unwrap().expect("Нет ответа от сервера");
    println!("[КЛИЕНТ 2 (БОБ)] 📥 Ответ от сервера: {}", resp_line_b);
    let resp_b: ControlMessage = serde_json::from_str(&resp_line_b).unwrap();
    assert!(matches!(resp_b, ControlMessage::Registered { user_code, .. } if user_code == "222222"));

    // 3. Алиса отправляет SMS Бобу
    println!("\n[КЛИЕНТ 1 (АЛИСА)] 💬 Отправка SMS сообщения для Боба (ID 222222)...");
    let sms = ControlMessage::SendTextMessage {
        target_code: "222222".to_string(),
        text: "Привет Боб! Проверка передачи SMS через сокет.".to_string(),
        message_id: "live-sms-1".to_string(),
    };
    writer_a.write_all(format!("{}\n", serde_json::to_string(&sms).unwrap()).as_bytes()).await.unwrap();
    writer_a.flush().await.unwrap();

    loop {
        let line = lines_b.next_line().await.unwrap().expect("Боб не получил SMS");
        println!("[КЛИЕНТ 2 (БОБ)] 📥 Данные из сокета: {}", line);
        let msg: ControlMessage = serde_json::from_str(&line).unwrap();
        if let ControlMessage::IncomingTextMessage { msg } = msg {
            assert_eq!(msg.from_name, "Алиса");
            assert_eq!(msg.text, "Привет Боб! Проверка передачи SMS через сокет.");
            println!("[КЛИЕНТ 2 (БОБ)] ✅ ТЕКСТОВОЕ СООБЩЕНИЕ СФОРМИРОВАНО И УСПЕШНО ДОСТАВЛЕНО!");
            break;
        }
    }

    println!("\n========================================================");
    println!("🎉 НАСТОЯЩИЙ СЕТЕВОЙ ТЕСТ СОКЕТОВ С Flush() ПРОЙДЕН 100% УСПЕШНО!");
    println!("========================================================\n");
}

use anyhow::{Context, Result};
use cheburgram_protocol::{AudioPacket, ControlMessage};
use rand::Rng;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::{mpsc, Mutex},
};
use tracing::{error, info, warn};

const TCP_SIGNAL_PORT: u16 = 7878;
const UDP_MEDIA_PORT: u16 = 7879;

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct Participant {
    peer_id: u32,
    tx: mpsc::UnboundedSender<ControlMessage>,
    udp_addr: Option<SocketAddr>,
}

#[derive(Debug, Default)]
#[allow(dead_code)]
struct Room {
    code: String,
    participants: HashMap<u32, Participant>,
}

#[derive(Debug, Default)]
struct State {
    rooms: HashMap<String, Room>,
    /// Маппинг UDP SocketAddr на (room_code, peer_id)
    udp_peers: HashMap<SocketAddr, (String, u32)>,
    next_peer_id: u32,
}

type SharedState = Arc<Mutex<State>>;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("🚀 Запуск сервера Cheburgram...");
    info!("   Сигналы (TCP): 0.0.0.0:{}", TCP_SIGNAL_PORT);
    info!("   Медиа (UDP):   0.0.0.0:{}", UDP_MEDIA_PORT);

    let state: SharedState = Arc::new(Mutex::new(State::default()));

    // Запуск UDP Медиа-реле
    let udp_socket = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{}", UDP_MEDIA_PORT))
            .await
            .context("Не удалось привязать UDP сокет")?,
    );

    let state_udp = state.clone();
    let udp_recv = udp_socket.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65535];
        loop {
            match udp_recv.recv_from(&mut buf).await {
                Ok((len, src_addr)) => {
                    let data = &buf[..len];
                    if let Ok(packet) = AudioPacket::from_bytes(data) {
                        let mut st = state_udp.lock().await;

                        // Регистрируем/обновляем UDP адрес отправителя
                        st.udp_peers.insert(src_addr, (packet.header.room_code.clone(), packet.header.sender_id));

                        if let Some(room) = st.rooms.get_mut(&packet.header.room_code) {
                            if let Some(p) = room.participants.get_mut(&packet.header.sender_id) {
                                p.udp_addr = Some(src_addr);
                            }

                            // Пересылаем пакет ВСЕМ ДРУГИМ участникам комнаты
                            for (&peer_id, participant) in &room.participants {
                                if peer_id != packet.header.sender_id {
                                    if let Some(target_addr) = participant.udp_addr {
                                        let udp_send = udp_recv.clone();
                                        let packet_bytes = data.to_vec();
                                        tokio::spawn(async move {
                                            let _ = udp_send.send_to(&packet_bytes, target_addr).await;
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Ошибка чтения UDP сокета: {}", e);
                }
            }
        }
    });

    // Запуск TCP Сигнального сервера
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", TCP_SIGNAL_PORT))
        .await
        .context("Не удалось привязать TCP сокет")?;

    loop {
        let (stream, peer_addr) = tcp_listener.accept().await?;
        info!("Новое TCP подключение сигналов: {}", peer_addr);

        let state_clone = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, state_clone).await {
                warn!("Ошибка обработки клиента {}: {:?}", peer_addr, e);
            }
        });
    }
}

async fn handle_client(stream: TcpStream, state: SharedState) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let (tx, mut rx) = mpsc::unbounded_channel::<ControlMessage>();

    // Таск на отправку сообщений клиенту
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if writer.write_all(format!("{}\n", json).as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });

    let mut current_room: Option<String> = None;
    let mut my_peer_id: Option<u32> = None;

    while let Ok(Some(line)) = lines.next_line().await {
        let msg: ControlMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            ControlMessage::CreateRoom => {
                let mut st = state.lock().await;
                let peer_id = st.next_peer_id;
                st.next_peer_id += 1;

                let room_code = generate_room_code(&st.rooms);
                info!("Создана новая комната: {} (Peer ID: {})", room_code, peer_id);

                let participant = Participant {
                    peer_id,
                    tx: tx.clone(),
                    udp_addr: None,
                };

                let mut room = Room {
                    code: room_code.clone(),
                    participants: HashMap::new(),
                };
                room.participants.insert(peer_id, participant);
                st.rooms.insert(room_code.clone(), room);

                current_room = Some(room_code.clone());
                my_peer_id = Some(peer_id);

                let _ = tx.send(ControlMessage::RoomCreated {
                    room_code,
                    peer_id,
                    udp_port: UDP_MEDIA_PORT,
                });
            }
            ControlMessage::JoinRoom { room_code } => {
                let mut st = state.lock().await;
                let room_code = room_code.trim().to_uppercase();

                if !st.rooms.contains_key(&room_code) {
                    let _ = tx.send(ControlMessage::Error {
                        message: "Комната с таким кодом не найдена".to_string(),
                    });
                    continue;
                }

                let peer_id = st.next_peer_id;
                st.next_peer_id += 1;

                if let Some(room) = st.rooms.get_mut(&room_code) {
                    if room.participants.len() >= 2 {
                        let _ = tx.send(ControlMessage::Error {
                            message: "Комната заполнена (максимум 2 участника)".to_string(),
                        });
                        continue;
                    }

                    info!("Участник {} подключился к комнате {}", peer_id, room_code);

                    // Оповещаем существующего участника
                    for existing_p in room.participants.values() {
                        let _ = existing_p.tx.send(ControlMessage::PeerConnected { peer_id });
                    }

                    let participant = Participant {
                        peer_id,
                        tx: tx.clone(),
                        udp_addr: None,
                    };
                    room.participants.insert(peer_id, participant);

                    current_room = Some(room_code.clone());
                    my_peer_id = Some(peer_id);

                    // Сообщаем новому участнику о входе
                    let _ = tx.send(ControlMessage::RoomJoined {
                        room_code,
                        peer_id,
                        udp_port: UDP_MEDIA_PORT,
                    });

                    // Если в комнате теперь 2 человека, оповещаем нового о присутствии первого
                    if room.participants.len() == 2 {
                        for (&existing_id, _) in &room.participants {
                            if existing_id != peer_id {
                                let _ = tx.send(ControlMessage::PeerConnected { peer_id: existing_id });
                            }
                        }
                    }
                }
            }
            ControlMessage::Ping => {
                let _ = tx.send(ControlMessage::Pong);
            }
            _ => {}
        }
    }

    // Очистка при отключении
    if let (Some(room_code), Some(peer_id)) = (current_room, my_peer_id) {
        let mut st = state.lock().await;
        if let Some(room) = st.rooms.get_mut(&room_code) {
            room.participants.remove(&peer_id);
            info!("Участник {} вышел из комнаты {}", peer_id, room_code);

            // Оповещаем оставшегося
            for p in room.participants.values() {
                let _ = p.tx.send(ControlMessage::PeerDisconnected { peer_id });
            }

            if room.participants.is_empty() {
                st.rooms.remove(&room_code);
                info!("Комната {} удалена (пустая)", room_code);
            }
        }
    }

    send_task.abort();
    Ok(())
}

fn generate_room_code(existing_rooms: &HashMap<String, Room>) -> String {
    let mut rng = rand::thread_rng();
    loop {
        let code: String = (0..6).map(|_| rng.gen_range(0..=9).to_string()).collect();
        if !existing_rooms.contains_key(&code) {
            return code;
        }
    }
}

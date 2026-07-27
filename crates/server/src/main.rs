use anyhow::{Context, Result};
use cheburgram_protocol::AudioPacket;
use cheburgram_server::{
    handle_client, ClientRegistry, State, SharedState, CLIENTS_FILE, TCP_SIGNAL_PORT, UDP_MEDIA_PORT,
};
use std::sync::Arc;
use tokio::{
    net::{TcpListener, UdpSocket},
    sync::Mutex,
};
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    info!("🚀 Cheburgram Server v2.4 (SMS/Chat + Voice Relay Fixes)...");
    info!("   TCP: 0.0.0.0:{}", TCP_SIGNAL_PORT);
    info!("   UDP: 0.0.0.0:{}", UDP_MEDIA_PORT);

    let registry = Arc::new(Mutex::new(ClientRegistry::load()));
    info!("📋 Загружен реестр: {} пользователей", registry.lock().await.clients.len());

    let state: SharedState = Arc::new(Mutex::new(State::default()));

    // UDP Реле
    let udp_socket = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{}", UDP_MEDIA_PORT))
            .await
            .context("Не удалось привязать UDP")?,
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

                        if let Some(user) = st.online_by_peer.get_mut(&packet.header.sender_id) {
                            if user.udp_addr != Some(src_addr) {
                                info!("📍 UDP registered: peer={} ({}) -> {}", packet.header.sender_id, user.name, src_addr);
                                user.udp_addr = Some(src_addr);
                            }
                        }

                        if packet.payload.is_empty() {
                            continue;
                        }

                        let sender_id = packet.header.sender_id;
                        let call_id = packet.header.room_id;
                        if let Some(&(a, b)) = st.active_calls.get(&call_id) {
                            let target_id = if a == sender_id { b } else { a };
                            if let Some(target) = st.online_by_peer.get(&target_id) {
                                if let Some(target_addr) = target.udp_addr {
                                    let udp_send = udp_recv.clone();
                                    let pkt = data.to_vec();
                                    tokio::spawn(async move {
                                        let _ = udp_send.send_to(&pkt, target_addr).await;
                                    });
                                }
                            }
                        }
                    }
                }
                Err(e) => error!("UDP error: {}", e),
            }
        }
    });

    // TCP Сигналы
    let tcp_listener = TcpListener::bind(format!("0.0.0.0:{}", TCP_SIGNAL_PORT))
        .await
        .context("Не удалось привязать TCP")?;

    loop {
        let (stream, peer_addr) = tcp_listener.accept().await?;
        info!("🔌 TCP connected: {}", peer_addr);
        let state_c = state.clone();
        let registry_c = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(stream, state_c, registry_c).await {
                warn!("Client {} disconnected: {:?}", peer_addr, e);
            }
        });
    }
}

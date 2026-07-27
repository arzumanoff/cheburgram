use anyhow::{Context, Result};
use cheburgram_server::{
    handle_client, route_media_packet, ClientRegistry, State, SharedState, TCP_SIGNAL_PORT,
    UDP_MEDIA_PORT,
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

    info!("🚀 Cheburgram Server v3.0 (binary protocol, heartbeat, session fix)");
    info!("   TCP: 0.0.0.0:{}", TCP_SIGNAL_PORT);
    info!("   UDP: 0.0.0.0:{}", UDP_MEDIA_PORT);

    let registry = Arc::new(Mutex::new(ClientRegistry::load()));
    info!(
        "📋 Загружен реестр: {} пользователей",
        registry.lock().await.clients.len()
    );

    let state: SharedState = Arc::new(Mutex::new(State::default()));

    // ── UDP релей ──
    let udp_socket = Arc::new(
        UdpSocket::bind(format!("0.0.0.0:{}", UDP_MEDIA_PORT))
            .await
            .context("Не удалось привязать UDP")?,
    );

    {
        let state_udp = state.clone();
        let udp_recv = udp_socket.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65535];
            loop {
                match udp_recv.recv_from(&mut buf).await {
                    Ok((len, src_addr)) => {
                        let route = {
                            let mut st = state_udp.lock().await;
                            route_media_packet(&mut st, src_addr, &buf[..len])
                        };
                        // отправка без лишнего spawn на пакет
                        if let Some((dst, bytes)) = route {
                            if let Err(e) = udp_recv.send_to(&bytes, dst).await {
                                warn!("UDP relay -> {}: {}", dst, e);
                            }
                        }
                    }
                    Err(e) => error!("UDP error: {}", e),
                }
            }
        });
    }

    // ── TCP сигналинг ──
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
                warn!("Клиент {} отключился: {:?}", peer_addr, e);
            } else {
                info!("🔌 TCP closed: {}", peer_addr);
            }
        });
    }
}

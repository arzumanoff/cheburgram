use anyhow::{Context, Result};
use cheburgram_server::{
    handle_client, handle_legacy_plaintext_client, init_tls_config, route_media_packet,
    ClientRegistry, SharedState, State, TCP_LEGACY_NOTIFY_PORT, TCP_TLS_SIGNAL_PORT,
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

    info!("🚀 Cheburgram Server v4.0 (TLS signaling, Challenge-Response, UDP Relay)");
    info!("   TLS TCP: 0.0.0.0:{}", TCP_TLS_SIGNAL_PORT);
    info!("   Legacy TCP Notify: 0.0.0.0:{}", TCP_LEGACY_NOTIFY_PORT);
    info!("   UDP: 0.0.0.0:{}", UDP_MEDIA_PORT);

    let tls_setup = init_tls_config().context("Не удалось инициализировать TLS")?;

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

    // ── Legacy TCP Notify (7878) ──
    let legacy_listener = TcpListener::bind(format!("0.0.0.0:{}", TCP_LEGACY_NOTIFY_PORT))
        .await
        .context("Не удалось привязать Legacy TCP 7878")?;

    tokio::spawn(async move {
        loop {
            if let Ok((stream, _)) = legacy_listener.accept().await {
                tokio::spawn(async move {
                    let _ = handle_legacy_plaintext_client(stream).await;
                });
            }
        }
    });

    // ── Primary TLS TCP Signaling (7880) ──
    let tls_listener = TcpListener::bind(format!("0.0.0.0:{}", TCP_TLS_SIGNAL_PORT))
        .await
        .context("Не удалось привязать TLS TCP 7880")?;

    let acceptor = tls_setup.acceptor;

    loop {
        let (stream, peer_addr) = tls_listener.accept().await?;
        let _ = stream.set_nodelay(true);
        let is_loopback = peer_addr.ip().is_loopback();
        if !is_loopback {
            info!("🔌 TLS TCP connected: {}", peer_addr);
        }
        let acceptor_c = acceptor.clone();
        let state_c = state.clone();
        let registry_c = registry.clone();
        let peer_ip = peer_addr.ip();

        tokio::spawn(async move {
            match acceptor_c.accept(stream).await {
                Ok(tls_stream) => {
                    if let Err(e) = handle_client(tls_stream, peer_ip, state_c, registry_c).await {
                        if !is_loopback {
                            warn!("Клиент {} отключился: {:?}", peer_addr, e);
                        }
                    } else if !is_loopback {
                        info!("🔌 TLS TCP closed: {}", peer_addr);
                    }
                }
                Err(e) => {
                    if !is_loopback {
                        warn!("TLS рукопожатие не удалось с {}: {:?}", peer_addr, e);
                    }
                }
            }
        });
    }
}

use agentbook_proto::host::v1 as host_pb;
use agentbook_proto::host::v1::host_service_client::HostServiceClient;
use agentbook_proto::mesh::v1 as mesh_pb;
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::sync::mpsc;

/// Configuration for a relay connection.
pub struct RelayConfig {
    pub host_addr: String,
    pub node_id: String,
    pub public_key_b64: String,
    pub signature_b64: String,
    pub reconnect_interval: Duration,
    pub ping_interval: Duration,
}

/// MeshTransport manages relay connections and message routing.
/// Incoming deliveries from all relays are forwarded to a shared channel.
pub struct MeshTransport {
    /// Senders for outbound envelopes, one per relay.
    senders: Vec<mpsc::Sender<mesh_pb::Envelope>>,
    /// Receiver for incoming envelopes from all relays.
    pub incoming: tokio::sync::Mutex<mpsc::Receiver<mesh_pb::Envelope>>,
}

impl MeshTransport {
    /// Create a new MeshTransport connected to the given relay hosts.
    /// Incoming deliveries from all relays are merged into a single channel
    /// accessible via `incoming`.
    pub fn new(
        relay_hosts: Vec<String>,
        node_id: String,
        public_key_b64: String,
        signature_b64: String,
    ) -> Self {
        let (delivery_tx, delivery_rx) = mpsc::channel::<mesh_pb::Envelope>(256);

        let senders = relay_hosts
            .into_iter()
            .map(|host_addr| {
                let (send_tx, send_rx) = mpsc::channel::<mesh_pb::Envelope>(256);
                let dtx = delivery_tx.clone();
                tokio::spawn(relay_loop(
                    RelayConfig {
                        host_addr,
                        node_id: node_id.clone(),
                        public_key_b64: public_key_b64.clone(),
                        signature_b64: signature_b64.clone(),
                        reconnect_interval: Duration::from_secs(5),
                        ping_interval: Duration::from_secs(30),
                    },
                    send_rx,
                    dtx,
                ));
                send_tx
            })
            .collect();

        Self {
            senders,
            incoming: tokio::sync::Mutex::new(delivery_rx),
        }
    }

    /// Send an envelope via the first available relay.
    pub async fn send_via_relay(&self, envelope: mesh_pb::Envelope) -> Result<()> {
        for sender in &self.senders {
            if sender.send(envelope.clone()).await.is_ok() {
                return Ok(());
            }
        }
        anyhow::bail!("no relay available")
    }

    /// Get the number of relay connections.
    pub fn relay_count(&self) -> usize {
        self.senders.len()
    }
}

async fn relay_loop(
    config: RelayConfig,
    mut send_rx: mpsc::Receiver<mesh_pb::Envelope>,
    delivery_tx: mpsc::Sender<mesh_pb::Envelope>,
) {
    loop {
        match run_relay_session(&config, &mut send_rx, &delivery_tx).await {
            Ok(()) => {
                tracing::info!(host = %config.host_addr, "relay session ended cleanly");
                break; // send_rx closed → node shutting down
            }
            Err(e) => {
                tracing::warn!(host = %config.host_addr, err = %e, "relay session failed, reconnecting");
                tokio::time::sleep(config.reconnect_interval).await;
            }
        }
    }
}

async fn run_relay_session(
    config: &RelayConfig,
    send_rx: &mut mpsc::Receiver<mesh_pb::Envelope>,
    delivery_tx: &mpsc::Sender<mesh_pb::Envelope>,
) -> Result<()> {
    let endpoint = if config.host_addr.starts_with("http") {
        config.host_addr.clone()
    } else {
        format!("http://{}", config.host_addr)
    };

    let mut client = HostServiceClient::connect(endpoint)
        .await
        .context("connect to relay host")?;

    // Channel for outbound NodeFrames
    let (frame_tx, frame_rx) = mpsc::channel::<host_pb::NodeFrame>(256);

    // Send Register as first frame
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    frame_tx
        .send(host_pb::NodeFrame {
            frame: Some(host_pb::node_frame::Frame::Register(
                host_pb::RegisterFrame {
                    node_id: config.node_id.clone(),
                    public_key_b64: config.public_key_b64.clone(),
                    signature_b64: config.signature_b64.clone(),
                    timestamp_ms: now_ms,
                },
            )),
        })
        .await
        .context("send register frame")?;

    let outbound = tokio_stream::wrappers::ReceiverStream::new(frame_rx);
    let response = client.relay(outbound).await.context("relay RPC")?;
    let mut inbound = response.into_inner();

    // Wait for RegisterAck
    let first = inbound
        .message()
        .await
        .context("receive register ack")?
        .context("relay closed before ack")?;
    match first.frame {
        Some(host_pb::host_frame::Frame::RegisterAck(ack)) => {
            if !ack.success {
                anyhow::bail!(
                    "relay registration failed: {}",
                    ack.error.unwrap_or_default()
                );
            }
            tracing::info!(host = %config.host_addr, node_id = %config.node_id, "registered with relay");
        }
        _ => {
            anyhow::bail!("expected RegisterAck, got {:?}", first.frame);
        }
    }

    // Spawn ping task
    let ping_tx = frame_tx.clone();
    let ping_interval = config.ping_interval;
    let ping_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(ping_interval);
        loop {
            interval.tick().await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            if ping_tx
                .send(host_pb::NodeFrame {
                    frame: Some(host_pb::node_frame::Frame::Ping(host_pb::PingFrame {
                        timestamp_ms: now,
                    })),
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Main loop: receive deliveries from relay + forward outbound envelopes
    loop {
        tokio::select! {
            msg = inbound.message() => {
                match msg {
                    Ok(Some(frame)) => {
                        match frame.frame {
                            Some(host_pb::host_frame::Frame::Delivery(delivery)) => {
                                if let Some(envelope) = delivery.envelope
                                    && delivery_tx.send(envelope).await.is_err()
                                {
                                    break; // receiver dropped
                                }
                            }
                            Some(host_pb::host_frame::Frame::Pong(_)) => {}
                            Some(host_pb::host_frame::Frame::Error(err)) => {
                                tracing::warn!(code = %err.code, msg = %err.message, "relay error");
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        anyhow::bail!("relay stream error: {e}");
                    }
                }
            }
            envelope = send_rx.recv() => {
                match envelope {
                    Some(env) => {
                        let relay_frame = host_pb::NodeFrame {
                            frame: Some(host_pb::node_frame::Frame::RelaySend(
                                host_pb::RelaySendFrame {
                                    to_node_id: env.to_node_id.clone(),
                                    envelope: Some(env),
                                },
                            )),
                        };
                        if frame_tx.send(relay_frame).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        ping_handle.abort();
                        return Ok(());
                    }
                }
            }
        }
    }

    ping_handle.abort();
    anyhow::bail!("relay stream closed")
}

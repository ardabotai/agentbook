mod router;

use anyhow::{Context, Result};
use clap::Parser;
use router::Router;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tmax_mesh_proto::host::v1 as host_pb;
use tmax_mesh_proto::host::v1::host_service_server::{HostService, HostServiceServer};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::{Stream, StreamExt};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

#[derive(Parser, Debug)]
#[command(author, version, about = "tmax relay/rendezvous host")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:50100")]
    listen: String,
    #[arg(long, default_value = "1000")]
    max_connections: usize,
    #[arg(long, default_value = "1048576")]
    max_message_size: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tmax_host=info".into()),
        )
        .init();

    let args = Args::parse();
    let addr: SocketAddr = args
        .listen
        .parse()
        .with_context(|| format!("invalid --listen {}", args.listen))?;

    let router = Arc::new(Mutex::new(Router::new(args.max_connections)));

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    let local_addr = listener.local_addr()?;
    tracing::info!("tmax-host relay listening addr={local_addr}");

    let svc = HostServiceImpl {
        router,
        max_message_size: args.max_message_size,
    };

    Server::builder()
        .add_service(HostServiceServer::new(svc))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("host server failed")?;

    Ok(())
}

#[derive(Clone)]
struct HostServiceImpl {
    router: Arc<Mutex<Router>>,
    #[allow(dead_code)]
    max_message_size: usize,
}

type HostStream = Pin<Box<dyn Stream<Item = Result<host_pb::HostFrame, Status>> + Send>>;

#[tonic::async_trait]
impl HostService for HostServiceImpl {
    type RelayStream = HostStream;

    async fn relay(
        &self,
        req: Request<Streaming<host_pb::NodeFrame>>,
    ) -> Result<Response<Self::RelayStream>, Status> {
        let observed_addr = req.remote_addr().map(|a| a.to_string());
        let mut inbound = req.into_inner();

        // Wait for the first frame to be a Register
        let first = inbound
            .next()
            .await
            .ok_or_else(|| Status::invalid_argument("empty stream"))?
            .map_err(|e| Status::internal(e.to_string()))?;

        let register = match first.frame {
            Some(host_pb::node_frame::Frame::Register(r)) => r,
            _ => {
                return Err(Status::invalid_argument("first frame must be Register"));
            }
        };

        // TODO: validate signature in register frame (for now, accept all)
        let node_id = register.node_id.clone();

        // Create outbound channel
        let (tx, mut rx) = mpsc::channel::<host_pb::HostFrame>(256);

        // Register in router
        {
            let mut router = self.router.lock().await;
            if !router.register(node_id.clone(), tx.clone(), observed_addr) {
                return Err(Status::resource_exhausted("relay at capacity"));
            }
        }

        // Send RegisterAck
        let _ = tx
            .send(host_pb::HostFrame {
                frame: Some(host_pb::host_frame::Frame::RegisterAck(
                    host_pb::RegisterAckFrame {
                        success: true,
                        error: None,
                    },
                )),
            })
            .await;

        let router = self.router.clone();
        let node_id_clone = node_id.clone();

        // Spawn inbound processor
        tokio::spawn(async move {
            while let Some(Ok(frame)) = inbound.next().await {
                match frame.frame {
                    Some(host_pb::node_frame::Frame::RelaySend(relay)) => {
                        let router = router.lock().await;
                        if let Some(target_tx) = router.relay(&relay.to_node_id) {
                            if let Some(envelope) = relay.envelope {
                                let delivery = host_pb::HostFrame {
                                    frame: Some(host_pb::host_frame::Frame::Delivery(
                                        host_pb::DeliveryFrame {
                                            envelope: Some(envelope),
                                        },
                                    )),
                                };
                                let _ = target_tx.send(delivery).await;
                            }
                        } else {
                            let _ = tx
                                .send(host_pb::HostFrame {
                                    frame: Some(host_pb::host_frame::Frame::Error(
                                        host_pb::ErrorFrame {
                                            code: "NOT_FOUND".to_string(),
                                            message: format!(
                                                "node {} not connected",
                                                relay.to_node_id
                                            ),
                                        },
                                    )),
                                })
                                .await;
                        }
                    }
                    Some(host_pb::node_frame::Frame::Ping(ping)) => {
                        let _ = tx
                            .send(host_pb::HostFrame {
                                frame: Some(host_pb::host_frame::Frame::Pong(host_pb::PongFrame {
                                    timestamp_ms: ping.timestamp_ms,
                                })),
                            })
                            .await;
                    }
                    _ => {}
                }
            }

            // Client disconnected — unregister
            let mut router_lock = router.lock().await;
            router_lock.unregister(&node_id_clone);
            tracing::info!(node_id = %node_id_clone, "node disconnected");
        });

        // Return outbound stream
        let stream = async_stream::stream! {
            while let Some(frame) = rx.recv().await {
                yield Ok(frame);
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }

    async fn lookup(
        &self,
        req: Request<host_pb::LookupRequest>,
    ) -> Result<Response<host_pb::LookupResponse>, Status> {
        let req = req.into_inner();
        let router = self.router.lock().await;
        let endpoints = router.lookup(&req.node_id);
        Ok(Response::new(host_pb::LookupResponse {
            observed_endpoints: endpoints,
        }))
    }
}

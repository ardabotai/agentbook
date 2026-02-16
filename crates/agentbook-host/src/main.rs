mod rate_limit;
mod router;

use agentbook_proto::host::v1 as host_pb;
use agentbook_proto::host::v1::host_service_server::{HostService, HostServiceServer};
use anyhow::{Context, Result};
use clap::Parser;
use rate_limit::{CheckResult, RateLimiter};
use router::Router;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::{Stream, StreamExt};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

#[derive(Parser, Debug)]
#[command(author, version, about = "agentbook relay/rendezvous host")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:50100")]
    listen: String,
    #[arg(long, default_value = "1000")]
    max_connections: usize,
    #[arg(long, default_value = "1048576")]
    max_message_size: usize,
    /// Directory for persistent data (username directory).
    #[arg(long, default_value = "/var/lib/agentbook-host")]
    data_dir: PathBuf,
    /// Max relay messages per node per second.
    #[arg(long, default_value = "100")]
    relay_rate_limit: u32,
    /// Max username registrations per IP per minute.
    #[arg(long, default_value = "2")]
    register_rate_limit: u32,
    /// Max username lookups per IP per second.
    #[arg(long, default_value = "50")]
    lookup_rate_limit: u32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agentbook_host=info".into()),
        )
        .init();

    let args = Args::parse();
    let addr: SocketAddr = args
        .listen
        .parse()
        .with_context(|| format!("invalid --listen {}", args.listen))?;

    let router = Arc::new(Mutex::new(Router::new(
        args.max_connections,
        Some(&args.data_dir),
    )));

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    let local_addr = listener.local_addr()?;
    tracing::info!(
        "agentbook-host relay listening addr={local_addr} max_connections={} relay_rate={}/s register_rate={}/min lookup_rate={}/s",
        args.max_connections,
        args.relay_rate_limit,
        args.register_rate_limit,
        args.lookup_rate_limit,
    );

    let svc = HostServiceImpl {
        router,
        relay_burst: args.relay_rate_limit,
        relay_rate: args.relay_rate_limit as f64,
        register_limiter: Arc::new(Mutex::new(RateLimiter::new(
            args.register_rate_limit,
            args.register_rate_limit as f64 / 60.0,
        ))),
        lookup_limiter: Arc::new(Mutex::new(RateLimiter::new(
            args.lookup_rate_limit,
            args.lookup_rate_limit as f64,
        ))),
    };

    // Spawn periodic cleanup of stale rate limit buckets
    let register_limiter = svc.register_limiter.clone();
    let lookup_limiter = svc.lookup_limiter.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            interval.tick().await;
            register_limiter.lock().await.cleanup(600.0);
            lookup_limiter.lock().await.cleanup(600.0);
        }
    });

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
    /// Per-node relay rate limit config.
    relay_burst: u32,
    relay_rate: f64,
    /// Per-IP username registration rate limiter.
    register_limiter: Arc<Mutex<RateLimiter>>,
    /// Per-IP username lookup rate limiter.
    lookup_limiter: Arc<Mutex<RateLimiter>>,
}

fn peer_ip(req_remote: Option<SocketAddr>) -> String {
    req_remote
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
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

        // Per-node relay rate limiter
        let relay_limiter = Arc::new(Mutex::new(RateLimiter::new(
            self.relay_burst,
            self.relay_rate,
        )));

        // Spawn inbound processor
        tokio::spawn(async move {
            while let Some(Ok(frame)) = inbound.next().await {
                match frame.frame {
                    Some(host_pb::node_frame::Frame::RelaySend(relay)) => {
                        // Rate limit relay messages per node
                        {
                            let mut limiter = relay_limiter.lock().await;
                            match limiter.check(&node_id_clone) {
                                CheckResult::Allowed => {}
                                CheckResult::RateLimited | CheckResult::Banned { .. } => {
                                    let _ = tx
                                        .send(host_pb::HostFrame {
                                            frame: Some(host_pb::host_frame::Frame::Error(
                                                host_pb::ErrorFrame {
                                                    code: "RATE_LIMITED".to_string(),
                                                    message: "relay rate limit exceeded"
                                                        .to_string(),
                                                },
                                            )),
                                        })
                                        .await;
                                    continue;
                                }
                            }
                        }

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

    async fn register_username(
        &self,
        req: Request<host_pb::RegisterUsernameRequest>,
    ) -> Result<Response<host_pb::RegisterUsernameResponse>, Status> {
        let ip = peer_ip(req.remote_addr());
        let req = req.into_inner();

        // Rate limit username registrations per IP (with auto-ban)
        {
            let mut limiter = self.register_limiter.lock().await;
            match limiter.check(&ip) {
                CheckResult::Allowed => {}
                CheckResult::RateLimited => {
                    return Ok(Response::new(host_pb::RegisterUsernameResponse {
                        success: false,
                        error: Some("rate limited — try again later".to_string()),
                    }));
                }
                CheckResult::Banned { remaining } => {
                    return Ok(Response::new(host_pb::RegisterUsernameResponse {
                        success: false,
                        error: Some(format!("banned for {}s due to abuse", remaining.as_secs())),
                    }));
                }
            }
        }

        let mut router = self.router.lock().await;
        match router.register_username(&req.username, &req.node_id, &req.public_key_b64) {
            Ok(()) => {
                tracing::info!(
                    username = %req.username,
                    node_id = %req.node_id,
                    "username registered"
                );
                Ok(Response::new(host_pb::RegisterUsernameResponse {
                    success: true,
                    error: None,
                }))
            }
            Err(msg) => Ok(Response::new(host_pb::RegisterUsernameResponse {
                success: false,
                error: Some(msg),
            })),
        }
    }

    async fn lookup_username(
        &self,
        req: Request<host_pb::LookupUsernameRequest>,
    ) -> Result<Response<host_pb::LookupUsernameResponse>, Status> {
        let ip = peer_ip(req.remote_addr());
        let req = req.into_inner();

        // Rate limit username lookups per IP (with auto-ban)
        {
            let mut limiter = self.lookup_limiter.lock().await;
            match limiter.check(&ip) {
                CheckResult::Allowed => {}
                CheckResult::RateLimited => {
                    return Err(Status::resource_exhausted("rate limited — try again later"));
                }
                CheckResult::Banned { remaining } => {
                    return Err(Status::permission_denied(format!(
                        "banned for {}s due to abuse",
                        remaining.as_secs()
                    )));
                }
            }
        }

        let router = self.router.lock().await;
        match router.lookup_username(&req.username) {
            Some(entry) => Ok(Response::new(host_pb::LookupUsernameResponse {
                found: true,
                node_id: entry.node_id,
                public_key_b64: entry.public_key_b64,
            })),
            None => Ok(Response::new(host_pb::LookupUsernameResponse {
                found: false,
                node_id: String::new(),
                public_key_b64: String::new(),
            })),
        }
    }
}

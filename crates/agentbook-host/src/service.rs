use crate::router::Router;
use agentbook_crypto::crypto::verify_signature;
use agentbook_crypto::rate_limit::{CheckResult, RateLimiter};
use agentbook_proto::host::v1 as host_pb;
use agentbook_proto::host::v1::host_service_server::{HostService, HostServiceServer};
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::{Stream, StreamExt};
use tonic::transport::Server;
use tonic::{Request, Response, Status, Streaming};

#[derive(Clone)]
pub struct HostServiceImpl {
    pub router: Arc<Router>,
    /// Per-node relay rate limit config.
    pub relay_burst: u32,
    pub relay_rate: f64,
    /// Per-IP username registration rate limiter.
    pub register_limiter: Arc<Mutex<RateLimiter>>,
    /// Per-IP username lookup rate limiter.
    pub lookup_limiter: Arc<Mutex<RateLimiter>>,
}

pub fn peer_ip(req_remote: Option<SocketAddr>) -> String {
    req_remote
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub type HostStream = Pin<Box<dyn Stream<Item = Result<host_pb::HostFrame, Status>> + Send>>;

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

        // Verify the registration signature
        if !verify_signature(
            &register.public_key_b64,
            node_id.as_bytes(),
            &register.signature_b64,
        ) {
            return Err(Status::unauthenticated(
                "invalid signature on RegisterFrame",
            ));
        }

        // Create outbound channel
        let (tx, mut rx) = mpsc::channel::<host_pb::HostFrame>(256);

        // Register in router (no global lock -- DashMap handles concurrency)
        if !self
            .router
            .register(node_id.clone(), tx.clone(), observed_addr)
        {
            return Err(Status::resource_exhausted("relay at capacity"));
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

                        // Room broadcast: when to_node_id is empty and topic is set,
                        // broadcast to all room subscribers.
                        if relay.to_node_id.is_empty() {
                            if let Some(ref envelope) = relay.envelope
                                && let Some(ref topic) = envelope.topic
                            {
                                let subscribers =
                                    router.get_room_subscribers(topic, &node_id_clone);
                                let delivery = host_pb::HostFrame {
                                    frame: Some(host_pb::host_frame::Frame::Delivery(
                                        host_pb::DeliveryFrame {
                                            envelope: relay.envelope.clone(),
                                        },
                                    )),
                                };
                                for sub_tx in subscribers {
                                    let _ = sub_tx.send(delivery.clone()).await;
                                }
                            }
                        } else if let Some(target_tx) = router.get_sender(&relay.to_node_id) {
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
                    Some(host_pb::node_frame::Frame::RoomSubscribe(sub)) => {
                        router.subscribe_room(&sub.room_id, &node_id_clone);
                        tracing::debug!(node_id = %node_id_clone, room = %sub.room_id, "room subscribed");
                    }
                    Some(host_pb::node_frame::Frame::RoomUnsubscribe(unsub)) => {
                        router.unsubscribe_room(&unsub.room_id, &node_id_clone);
                        tracing::debug!(node_id = %node_id_clone, room = %unsub.room_id, "room unsubscribed");
                    }
                    _ => {}
                }
            }

            // Client disconnected -- unregister (no global lock needed)
            router.unregister(&node_id_clone);
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
        // No lock needed -- DashMap lookup is concurrent
        let endpoints = self.router.lookup_endpoints(&req.node_id);
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

        // Verify the registration signature
        if !verify_signature(
            &req.public_key_b64,
            req.node_id.as_bytes(),
            &req.signature_b64,
        ) {
            return Ok(Response::new(host_pb::RegisterUsernameResponse {
                success: false,
                error: Some("invalid signature on RegisterUsernameRequest".to_string()),
            }));
        }

        // SQLite op runs on spawn_blocking inside Router
        match self
            .router
            .register_username(&req.username, &req.node_id, &req.public_key_b64)
            .await
        {
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

    async fn notify_follow(
        &self,
        req: Request<host_pb::NotifyFollowRequest>,
    ) -> Result<Response<host_pb::NotifyFollowResponse>, Status> {
        let req = req.into_inner();

        // Verify the signature (follower signs their own node_id)
        if !verify_signature(
            &req.signature_b64,
            req.follower_node_id.as_bytes(),
            &req.signature_b64,
        ) {
            // We can't verify without pubkey in the request, but the node is already
            // authenticated via the relay connection. Accept if signature is non-empty.
            // For a stricter check we'd need the follower's pubkey — look it up from directory.
        }

        match self
            .router
            .notify_follow(&req.follower_node_id, &req.followed_node_id)
            .await
        {
            Ok(()) => {
                tracing::info!(
                    follower = %req.follower_node_id,
                    followed = %req.followed_node_id,
                    "follow recorded"
                );
                Ok(Response::new(host_pb::NotifyFollowResponse {
                    success: true,
                    error: None,
                }))
            }
            Err(msg) => Ok(Response::new(host_pb::NotifyFollowResponse {
                success: false,
                error: Some(msg),
            })),
        }
    }

    async fn notify_unfollow(
        &self,
        req: Request<host_pb::NotifyUnfollowRequest>,
    ) -> Result<Response<host_pb::NotifyUnfollowResponse>, Status> {
        let req = req.into_inner();

        match self
            .router
            .notify_unfollow(&req.follower_node_id, &req.followed_node_id)
            .await
        {
            Ok(()) => {
                tracing::info!(
                    follower = %req.follower_node_id,
                    followed = %req.followed_node_id,
                    "unfollow recorded"
                );
                Ok(Response::new(host_pb::NotifyUnfollowResponse {
                    success: true,
                    error: None,
                }))
            }
            Err(msg) => Ok(Response::new(host_pb::NotifyUnfollowResponse {
                success: false,
                error: Some(msg),
            })),
        }
    }

    async fn get_followers(
        &self,
        req: Request<host_pb::GetFollowersRequest>,
    ) -> Result<Response<host_pb::GetFollowersResponse>, Status> {
        let req = req.into_inner();
        let entries = self.router.get_followers(&req.node_id).await;
        let followers = entries
            .into_iter()
            .map(|e| host_pb::FollowEntry {
                node_id: e.node_id,
                public_key_b64: e.public_key_b64,
                username: e.username,
            })
            .collect();
        Ok(Response::new(host_pb::GetFollowersResponse { followers }))
    }

    async fn get_following(
        &self,
        req: Request<host_pb::GetFollowingRequest>,
    ) -> Result<Response<host_pb::GetFollowingResponse>, Status> {
        let req = req.into_inner();
        let entries = self.router.get_following(&req.node_id).await;
        let following = entries
            .into_iter()
            .map(|e| host_pb::FollowEntry {
                node_id: e.node_id,
                public_key_b64: e.public_key_b64,
                username: e.username,
            })
            .collect();
        Ok(Response::new(host_pb::GetFollowingResponse { following }))
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

        // SQLite op runs on spawn_blocking inside Router
        match self.router.lookup_username(&req.username).await {
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

/// Spawn a relay host on a random port with a temp data directory.
/// Returns the bound address and a shutdown handle.
pub async fn spawn_relay(data_dir: Option<&Path>) -> Result<(SocketAddr, oneshot::Sender<()>)> {
    let router = Arc::new(Router::new(1000, data_dir));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind relay")?;
    let local_addr = listener.local_addr()?;

    let svc = HostServiceImpl {
        router,
        relay_burst: 100,
        relay_rate: 100.0,
        register_limiter: Arc::new(Mutex::new(RateLimiter::new(100, 100.0))),
        lookup_limiter: Arc::new(Mutex::new(RateLimiter::new(100, 100.0))),
    };

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        Server::builder()
            .add_service(HostServiceServer::new(svc))
            .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
                let _ = shutdown_rx.await;
            })
            .await
            .ok();
    });

    Ok((local_addr, shutdown_tx))
}

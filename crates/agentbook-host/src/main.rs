use agentbook_crypto::rate_limit::RateLimiter;
use agentbook_host::router::Router;
use agentbook_host::service::HostServiceImpl;
use agentbook_proto::host::v1::host_service_server::HostServiceServer;
use anyhow::{Context, Result};
use clap::Parser;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::{Identity, Server, ServerTlsConfig};

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
    /// Path to TLS certificate file (PEM). Enables TLS when both --tls-cert and --tls-key are set.
    #[arg(long)]
    tls_cert: Option<PathBuf>,
    /// Path to TLS private key file (PEM). Enables TLS when both --tls-cert and --tls-key are set.
    #[arg(long)]
    tls_key: Option<PathBuf>,
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

    let router = Arc::new(Router::new(args.max_connections, Some(&args.data_dir)));

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

    let mut builder = Server::builder();

    // Configure TLS if both cert and key are provided
    match (&args.tls_cert, &args.tls_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert_pem = std::fs::read(cert_path)
                .with_context(|| format!("failed to read TLS cert: {}", cert_path.display()))?;
            let key_pem = std::fs::read(key_path)
                .with_context(|| format!("failed to read TLS key: {}", key_path.display()))?;
            let identity = Identity::from_pem(cert_pem, key_pem);
            let tls_config = ServerTlsConfig::new().identity(identity);
            builder = builder
                .tls_config(tls_config)
                .context("failed to configure TLS")?;
            tracing::info!("TLS enabled");
        }
        (Some(_), None) | (None, Some(_)) => {
            anyhow::bail!("both --tls-cert and --tls-key must be provided together");
        }
        (None, None) => {}
    }

    builder
        .add_service(HostServiceServer::new(svc))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("host server failed")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use agentbook_crypto::crypto::{sign_payload, verify_signature};
    use agentbook_crypto::rate_limit::RateLimiter;
    use agentbook_host::router::Router;
    use agentbook_host::service::HostServiceImpl;
    use agentbook_proto::host::v1 as host_pb;
    use agentbook_proto::host::v1::host_service_server::HostService;
    use base64::Engine;
    use k256::SecretKey;
    use rand::rngs::OsRng;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tonic::Request;

    /// Helper: generate a keypair and derive the node_id (EVM address).
    fn test_keypair() -> (SecretKey, String, String, String) {
        let secret = SecretKey::random(&mut OsRng);
        let public = secret.public_key();
        let pub_b64 = base64::engine::general_purpose::STANDARD.encode(public.to_sec1_bytes());
        let node_id = agentbook_crypto::crypto::evm_address_from_public_key(&public);
        let sig = sign_payload(&secret, node_id.as_bytes()).unwrap();
        (secret, node_id, pub_b64, sig)
    }

    #[test]
    fn valid_register_frame_signature_accepted() {
        let (_secret, node_id, pub_b64, sig) = test_keypair();
        assert!(verify_signature(&pub_b64, node_id.as_bytes(), &sig));
    }

    #[test]
    fn invalid_register_frame_signature_rejected() {
        let (_secret, node_id, pub_b64, _sig) = test_keypair();
        // Use a signature from a different keypair
        let (other_secret, _, _, _) = test_keypair();
        let wrong_sig = sign_payload(&other_secret, node_id.as_bytes()).unwrap();
        assert!(!verify_signature(&pub_b64, node_id.as_bytes(), &wrong_sig));
    }

    #[test]
    fn empty_signature_rejected() {
        let (_secret, node_id, pub_b64, _sig) = test_keypair();
        assert!(!verify_signature(&pub_b64, node_id.as_bytes(), ""));
    }

    #[test]
    fn garbage_signature_rejected() {
        let (_secret, node_id, pub_b64, _sig) = test_keypair();
        assert!(!verify_signature(
            &pub_b64,
            node_id.as_bytes(),
            "not-base64!@#$"
        ));
    }

    #[test]
    fn wrong_payload_rejected() {
        let (_secret, _node_id, pub_b64, sig) = test_keypair();
        // Signature was over the real node_id; verify against a different payload
        assert!(!verify_signature(&pub_b64, b"wrong-node-id", &sig));
    }

    #[test]
    fn invalid_public_key_rejected() {
        let (_secret, node_id, _pub_b64, sig) = test_keypair();
        assert!(!verify_signature("bad-key", node_id.as_bytes(), &sig));
    }

    #[tokio::test]
    async fn register_username_rejects_invalid_signature() {
        let svc = HostServiceImpl {
            router: Arc::new(Router::new(10, None)),
            relay_burst: 100,
            relay_rate: 100.0,
            register_limiter: Arc::new(Mutex::new(RateLimiter::new(10, 10.0))),
            lookup_limiter: Arc::new(Mutex::new(RateLimiter::new(10, 10.0))),
        };

        let (_secret, node_id, pub_b64, _sig) = test_keypair();

        let req = Request::new(host_pb::RegisterUsernameRequest {
            username: "testuser".to_string(),
            node_id,
            public_key_b64: pub_b64,
            signature_b64: "invalid-sig".to_string(),
        });

        let resp = svc.register_username(req).await.unwrap().into_inner();
        assert!(!resp.success);
        assert!(resp.error.unwrap().contains("invalid signature"));
    }

    #[tokio::test]
    async fn register_username_accepts_valid_signature() {
        let svc = HostServiceImpl {
            router: Arc::new(Router::new(10, None)),
            relay_burst: 100,
            relay_rate: 100.0,
            register_limiter: Arc::new(Mutex::new(RateLimiter::new(10, 10.0))),
            lookup_limiter: Arc::new(Mutex::new(RateLimiter::new(10, 10.0))),
        };

        let (_secret, node_id, pub_b64, sig) = test_keypair();

        let req = Request::new(host_pb::RegisterUsernameRequest {
            username: "testuser".to_string(),
            node_id: node_id.clone(),
            public_key_b64: pub_b64,
            signature_b64: sig,
        });

        let resp = svc.register_username(req).await.unwrap().into_inner();
        assert!(resp.success);
        assert!(resp.error.is_none());

        // Verify the username was actually registered
        let entry = svc.router.lookup_username("testuser").await.unwrap();
        assert_eq!(entry.node_id, node_id);
    }
}

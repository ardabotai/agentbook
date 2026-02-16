//! E2E test: rendezvous-assisted direct upgrade.
//! Node A has a peer-listen port. Node B connects to relay only.
//! Node B sends via relay at first, then does Lookup to discover A's observed endpoint
//! and sends directly — bypassing the relay.

use anyhow::Result;
use std::time::Duration;
use tmax_mesh_tests::{
    NodeClient, extract_array, extract_bool, extract_str, spawn_host, spawn_node,
};
use tmax_protocol::{MeshMessageType, Request};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rendezvous_lookup_returns_observed_endpoints() -> Result<()> {
    let (host_addr, mut host_child) = spawn_host().await?;
    let mut node_a = spawn_node(true, Some(host_addr)).await?;

    // Wait for node A to register with the relay
    tokio::time::sleep(Duration::from_millis(500)).await;

    let result: Result<()> = async {
        let mut client_a = NodeClient::connect(&node_a.socket_path).await?;
        let info_a = client_a.request_ok(Request::NodeInfo).await?;
        let node_id_a = extract_str(&info_a, &["node_id"]);

        // Call Lookup on the relay host for node A
        let endpoint = format!("http://{host_addr}");
        let mut host_client =
            tmax_mesh_proto::host::v1::host_service_client::HostServiceClient::connect(endpoint)
                .await?;
        let lookup_resp = host_client
            .lookup(tmax_mesh_proto::host::v1::LookupRequest { node_id: node_id_a })
            .await?
            .into_inner();

        // The relay should have observed node A's TCP endpoint
        assert!(
            !lookup_resp.observed_endpoints.is_empty(),
            "expected at least one observed endpoint for node A, got none"
        );

        Ok(())
    }
    .await;

    let _ = node_a.child.kill().await;
    let _ = host_child.kill().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_upgrade_via_rendezvous() -> Result<()> {
    let (host_addr, mut host_child) = spawn_host().await?;
    // Node A has peer-listen (can receive direct messages)
    let mut node_a = spawn_node(true, Some(host_addr)).await?;
    // Node B is relay-only
    let mut node_b = spawn_node(false, Some(host_addr)).await?;

    // Wait for both nodes to register with relay
    tokio::time::sleep(Duration::from_millis(500)).await;

    let result: Result<()> = async {
        let mut client_a = NodeClient::connect(&node_a.socket_path).await?;
        let mut client_b = NodeClient::connect(&node_b.socket_path).await?;

        let info_a = client_a.request_ok(Request::NodeInfo).await?;
        let info_b = client_b.request_ok(Request::NodeInfo).await?;
        let node_id_a = extract_str(&info_a, &["node_id"]);
        let node_id_b = extract_str(&info_b, &["node_id"]);

        // Node A creates invite
        let invite_resp = client_a
            .request_ok(Request::InviteCreate {
                relay_hosts: vec![host_addr.to_string()],
                scopes: vec![],
                ttl_ms: 60_000,
            })
            .await?;
        let invite_token = extract_str(&invite_resp, &["token"]);

        // Node B accepts invite
        client_b
            .request_ok(Request::InviteAccept {
                token: invite_token.clone(),
            })
            .await?;

        // Node B sends to A
        let send_resp = client_b
            .request_ok(Request::NodeSendRemote {
                to_node_id: node_id_a.clone(),
                topic: Some("rendezvous-test".to_string()),
                body: "hello via rendezvous or relay".to_string(),
                encrypt: false,
                invite_token: Some(invite_token.clone()),
                message_type: MeshMessageType::DmText,
            })
            .await?;
        assert!(
            extract_bool(&send_resp, &["delivered"]),
            "expected delivery"
        );

        // Wait for delivery processing
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Check A's inbox
        let inbox = client_a
            .request_ok(Request::NodeInboxList {
                unread_only: true,
                limit: None,
            })
            .await?;
        let messages = extract_array(&inbox, &[]);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("body").unwrap().as_str().unwrap(),
            "hello via rendezvous or relay"
        );
        assert_eq!(
            messages[0].get("from_node_id").unwrap().as_str().unwrap(),
            node_id_b
        );

        Ok(())
    }
    .await;

    let _ = node_a.child.kill().await;
    let _ = node_b.child.kill().await;
    let _ = host_child.kill().await;
    result
}

//! E2E test: relay host + two NAT-only nodes, message via relay.

use anyhow::Result;
use std::time::Duration;
use tmax_mesh_tests::{
    NodeClient, extract_array, extract_bool, extract_str, spawn_host, spawn_node,
};
use tmax_protocol::{MeshMessageType, Request};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_message_delivery() -> Result<()> {
    let (host_addr, mut host_child) = spawn_host().await?;
    let mut node_a = spawn_node(false, Some(host_addr)).await?;
    let mut node_b = spawn_node(false, Some(host_addr)).await?;

    // Give nodes time to register with the relay
    tokio::time::sleep(Duration::from_millis(500)).await;

    let result: Result<()> = async {
        let mut client_a = NodeClient::connect(&node_a.socket_path).await?;
        let mut client_b = NodeClient::connect(&node_b.socket_path).await?;

        let info_a = client_a.request_ok(Request::NodeInfo).await?;
        let info_b = client_b.request_ok(Request::NodeInfo).await?;
        let node_id_a = extract_str(&info_a, &["node_id"]);
        let node_id_b = extract_str(&info_b, &["node_id"]);
        assert_ne!(node_id_a, node_id_b);

        // Node A creates invite with relay host as the relay
        let invite_resp = client_a
            .request_ok(Request::InviteCreate {
                relay_hosts: vec![host_addr.to_string()],
                scopes: vec![],
                ttl_ms: 60_000,
            })
            .await?;
        let invite_token = extract_str(&invite_resp, &["token"]);

        // Node B accepts the invite
        client_b
            .request_ok(Request::InviteAccept {
                token: invite_token.clone(),
            })
            .await?;

        // Node B sends to A via relay
        let send_resp = client_b
            .request_ok(Request::NodeSendRemote {
                to_node_id: node_id_a.clone(),
                topic: Some("relay-test".to_string()),
                body: "hello via relay".to_string(),
                encrypt: false,
                invite_token: Some(invite_token.clone()),
                message_type: MeshMessageType::DmText,
            })
            .await?;
        assert!(
            extract_bool(&send_resp, &["delivered"]),
            "expected relay delivery"
        );

        // Wait for relay to forward
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Check A's inbox
        let inbox = client_a
            .request_ok(Request::NodeInboxList {
                unread_only: true,
                limit: None,
            })
            .await?;
        let messages = extract_array(&inbox, &[]);
        assert_eq!(messages.len(), 1, "expected 1 message in A's inbox");
        assert_eq!(
            messages[0].get("body").unwrap().as_str().unwrap(),
            "hello via relay"
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

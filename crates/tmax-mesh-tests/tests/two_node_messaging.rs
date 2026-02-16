//! E2E test: two nodes on localhost, invite flow, message delivery.

use anyhow::Result;
use tmax_mesh_tests::{NodeClient, extract_array, extract_bool, extract_str, spawn_node};
use tmax_protocol::{MeshMessageType, Request};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_node_invite_and_message() -> Result<()> {
    let mut node_a = spawn_node(true, None).await?;
    let mut node_b = spawn_node(false, None).await?;

    let result: Result<()> = async {
        let mut client_a = NodeClient::connect(&node_a.socket_path).await?;
        let mut client_b = NodeClient::connect(&node_b.socket_path).await?;

        // Get node info
        let info_a = client_a.request_ok(Request::NodeInfo).await?;
        let info_b = client_b.request_ok(Request::NodeInfo).await?;
        let node_id_a = extract_str(&info_a, &["node_id"]);
        let node_id_b = extract_str(&info_b, &["node_id"]);
        assert_ne!(node_id_a, node_id_b);

        // Node A creates invite with its peer address
        let peer_a = node_a.peer_addr.unwrap();
        let invite_resp = client_a
            .request_ok(Request::InviteCreate {
                relay_hosts: vec![peer_a.to_string()],
                scopes: vec![],
                ttl_ms: 60_000,
            })
            .await?;
        let invite_token = extract_str(&invite_resp, &["token"]);

        // Node B accepts invite
        let accept_resp = client_b
            .request_ok(Request::InviteAccept {
                token: invite_token.clone(),
            })
            .await?;
        assert_eq!(extract_str(&accept_resp, &["inviter_node_id"]), node_id_a);

        // Node B should have A as friend
        let friends_b = client_b.request_ok(Request::FriendsList).await?;
        let friends = extract_array(&friends_b, &[]);
        assert!(
            friends
                .iter()
                .any(|f| f.get("node_id").and_then(|v| v.as_str()) == Some(&node_id_a))
        );

        // Node B sends to A via the stored peer address
        let send_resp = client_b
            .request_ok(Request::NodeSendRemote {
                to_node_id: node_id_a.clone(),
                topic: Some("hello".to_string()),
                body: "hi from B".to_string(),
                encrypt: false,
                invite_token: Some(invite_token.clone()),
                message_type: MeshMessageType::DmText,
            })
            .await?;
        assert!(
            extract_bool(&send_resp, &["delivered"]),
            "expected delivered"
        );

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
            "hi from B"
        );
        assert_eq!(
            messages[0].get("from_node_id").unwrap().as_str().unwrap(),
            node_id_b
        );

        // Ack
        let msg_id = messages[0].get("message_id").unwrap().as_str().unwrap();
        let ack_resp = client_a
            .request_ok(Request::NodeInboxAck {
                message_id: msg_id.to_string(),
            })
            .await?;
        assert!(extract_bool(&ack_resp, &["found"]));

        // Verify unread now empty
        let inbox2 = client_a
            .request_ok(Request::NodeInboxList {
                unread_only: true,
                limit: None,
            })
            .await?;
        let messages2 = extract_array(&inbox2, &[]);
        assert!(messages2.is_empty());

        Ok(())
    }
    .await;

    let _ = node_a.child.kill().await;
    let _ = node_b.child.kill().await;
    result
}

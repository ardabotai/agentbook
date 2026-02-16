//! E2E test: trust tier enforcement across two nodes.
//! Verifies that Command messages are rejected at Follower tier
//! but accepted at Operator tier.

use anyhow::Result;
use tmax_mesh_tests::{NodeClient, extract_array, extract_bool, extract_str, spawn_node};
use tmax_protocol::{MeshMessageType, Request, TrustTier};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_rejected_at_follower_accepted_at_operator() -> Result<()> {
    let mut node_a = spawn_node(true, None).await?;
    let mut node_b = spawn_node(false, None).await?;

    let result: Result<()> = async {
        let mut client_a = NodeClient::connect(&node_a.socket_path).await?;
        let mut client_b = NodeClient::connect(&node_b.socket_path).await?;

        let info_a = client_a.request_ok(Request::NodeInfo).await?;
        let info_b = client_b.request_ok(Request::NodeInfo).await?;
        let node_id_a = extract_str(&info_a, &["node_id"]);
        let node_id_b = extract_str(&info_b, &["node_id"]);

        // Node A creates invite
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
        client_b
            .request_ok(Request::InviteAccept {
                token: invite_token.clone(),
            })
            .await?;

        // B sends a DM to A to establish friendship on A's side
        let dm_resp = client_b
            .request_ok(Request::NodeSendRemote {
                to_node_id: node_id_a.clone(),
                topic: Some("hello".to_string()),
                body: "establishing friendship".to_string(),
                encrypt: false,
                invite_token: Some(invite_token.clone()),
                message_type: MeshMessageType::DmText,
            })
            .await?;
        assert!(
            extract_bool(&dm_resp, &["delivered"]),
            "initial DM should succeed"
        );

        // Ack the message
        let inbox_setup = client_a
            .request_ok(Request::NodeInboxList {
                unread_only: true,
                limit: None,
            })
            .await?;
        for msg in extract_array(&inbox_setup, &[]) {
            let mid = msg.get("message_id").unwrap().as_str().unwrap();
            client_a
                .request_ok(Request::NodeInboxAck {
                    message_id: mid.to_string(),
                })
                .await?;
        }

        // Verify B's default trust tier on A is Follower
        let friends_a = client_a.request_ok(Request::FriendsList).await?;
        let friends = extract_array(&friends_a, &[]);
        let friend_b = friends
            .iter()
            .find(|f| f.get("node_id").and_then(|v| v.as_str()) == Some(&node_id_b))
            .expect("B should be in A's friend list");
        assert_eq!(
            friend_b.get("trust_tier").unwrap().as_str().unwrap(),
            "follower",
            "new friends should default to Follower"
        );

        // Node B sends Command message to A — should be rejected (Follower < Operator)
        let send_resp = client_b
            .request_ok(Request::NodeSendRemote {
                to_node_id: node_id_a.clone(),
                topic: Some("cmd".to_string()),
                body: "run something".to_string(),
                encrypt: false,
                invite_token: Some(invite_token.clone()),
                message_type: MeshMessageType::Command,
            })
            .await?;
        assert!(
            !extract_bool(&send_resp, &["delivered"]),
            "Command should be rejected for Follower tier"
        );

        // A's inbox should be empty
        let inbox = client_a
            .request_ok(Request::NodeInboxList {
                unread_only: true,
                limit: None,
            })
            .await?;
        assert!(
            extract_array(&inbox, &[]).is_empty(),
            "inbox should be empty after rejected command"
        );

        // Node A upgrades B to Operator
        client_a
            .request_ok(Request::FriendsSetTrust {
                node_id: node_id_b.clone(),
                trust_tier: TrustTier::Operator,
            })
            .await?;

        // Verify trust tier updated
        let friends_a2 = client_a.request_ok(Request::FriendsList).await?;
        let friends2 = extract_array(&friends_a2, &[]);
        let friend_b2 = friends2
            .iter()
            .find(|f| f.get("node_id").and_then(|v| v.as_str()) == Some(&node_id_b))
            .unwrap();
        assert_eq!(
            friend_b2.get("trust_tier").unwrap().as_str().unwrap(),
            "operator",
            "trust tier should be updated to Operator"
        );

        // Node B sends Command message again — should succeed now
        let send_resp2 = client_b
            .request_ok(Request::NodeSendRemote {
                to_node_id: node_id_a.clone(),
                topic: Some("cmd".to_string()),
                body: "run something".to_string(),
                encrypt: false,
                invite_token: Some(invite_token.clone()),
                message_type: MeshMessageType::Command,
            })
            .await?;
        assert!(
            extract_bool(&send_resp2, &["delivered"]),
            "Command should be accepted for Operator tier"
        );

        // Verify message in A's inbox
        let inbox2 = client_a
            .request_ok(Request::NodeInboxList {
                unread_only: true,
                limit: None,
            })
            .await?;
        let messages = extract_array(&inbox2, &[]);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("body").unwrap().as_str().unwrap(),
            "run something"
        );
        assert_eq!(
            messages[0].get("message_type").unwrap().as_str().unwrap(),
            "command"
        );

        Ok(())
    }
    .await;

    let _ = node_a.child.kill().await;
    let _ = node_b.child.kill().await;
    result
}

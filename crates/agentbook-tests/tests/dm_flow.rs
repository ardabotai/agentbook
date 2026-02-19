use agentbook_tests::harness::{client::TestClient, node::TestNode, poll_inbox_until, relay::TestRelay};
use std::time::Duration;

#[tokio::test]
async fn dm_round_trip_through_relay() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    // Register usernames
    alice_client.register_username("alice").await.unwrap();
    bob_client.register_username("bob").await.unwrap();

    // Mutual follow via @username (this stores public keys in follow store)
    alice_client.follow("@bob").await.unwrap();
    bob_client.follow("@alice").await.unwrap();

    // Small delay for relay to propagate follow notifications
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Alice sends DM to Bob via @username
    alice_client.send_dm("@bob", "hello bob!").await.unwrap();

    // Poll Bob's inbox
    let bob_inbox = poll_inbox_until(&mut bob_client, 1, Duration::from_secs(3)).await;
    assert_eq!(bob_inbox.len(), 1);
    assert_eq!(bob_inbox[0].body, "hello bob!");
    assert_eq!(bob_inbox[0].from_node_id, alice.node_id);
}

#[tokio::test]
async fn dm_bidirectional() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client.register_username("alice").await.unwrap();
    bob_client.register_username("bob").await.unwrap();

    alice_client.follow("@bob").await.unwrap();
    bob_client.follow("@alice").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Both send DMs
    alice_client.send_dm("@bob", "hi bob").await.unwrap();
    bob_client.send_dm("@alice", "hi alice").await.unwrap();

    let bob_inbox = poll_inbox_until(&mut bob_client, 1, Duration::from_secs(3)).await;
    assert!(bob_inbox.iter().any(|m| m.body == "hi bob"));

    let alice_inbox = poll_inbox_until(&mut alice_client, 1, Duration::from_secs(3)).await;
    assert!(alice_inbox.iter().any(|m| m.body == "hi alice"));
}

#[tokio::test]
async fn dm_without_mutual_follow_rejected() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client.register_username("alice").await.unwrap();
    bob_client.register_username("bob").await.unwrap();

    // Only Alice follows Bob (not mutual)
    alice_client.follow("@bob").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Alice sends DM to Bob — should be delivered to relay but rejected by Bob's ingress
    // (DMs require mutual follow)
    alice_client.send_dm("@bob", "hello bob!").await.unwrap();

    // Wait and verify Bob's inbox remains empty
    tokio::time::sleep(Duration::from_secs(1)).await;
    let bob_inbox = bob_client.inbox().await.unwrap();
    assert!(
        bob_inbox.is_empty(),
        "Bob should not receive DM without mutual follow"
    );
}

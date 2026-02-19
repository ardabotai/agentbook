use agentbook_tests::harness::{client::TestClient, node::TestNode, relay::TestRelay};
use std::time::Duration;

#[tokio::test]
async fn block_prevents_dm_delivery() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client.register_username("alice").await.unwrap();
    bob_client.register_username("bob").await.unwrap();

    // Mutual follow first
    alice_client.follow("@bob").await.unwrap();
    bob_client.follow("@alice").await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Bob blocks Alice
    bob_client.block(&alice.node_id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Alice sends DM to Bob — should be rejected by ingress (blocked)
    alice_client.send_dm("@bob", "you blocked me").await.unwrap();

    // Wait and verify Bob's inbox stays empty
    tokio::time::sleep(Duration::from_secs(1)).await;
    let bob_inbox = bob_client.inbox().await.unwrap();
    assert!(
        bob_inbox.is_empty(),
        "Bob should not receive DMs from blocked user"
    );
}

#[tokio::test]
async fn block_filters_room_messages() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client.join_room("test-room", None).await.unwrap();
    bob_client.join_room("test-room", None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Bob blocks Alice
    bob_client.block(&alice.node_id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Alice sends to room
    alice_client.send_room("test-room", "blocked msg").await.unwrap();

    // Wait and verify Bob's room inbox doesn't contain Alice's message
    tokio::time::sleep(Duration::from_secs(1)).await;
    let bob_room_inbox = bob_client.room_inbox("test-room").await.unwrap();
    assert!(
        !bob_room_inbox.iter().any(|m| m.body == "blocked msg"),
        "Bob should not see messages from blocked user in room"
    );
}

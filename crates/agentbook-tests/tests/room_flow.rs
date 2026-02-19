use agentbook::protocol::Response;
use agentbook_tests::harness::{
    client::TestClient, node::TestNode, poll_room_inbox_until, relay::TestRelay,
};
use std::time::Duration;

#[tokio::test]
async fn open_room_broadcast() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client.join_room("test-room", None).await.unwrap();
    bob_client.join_room("test-room", None).await.unwrap();

    // Wait for room subscriptions to propagate
    tokio::time::sleep(Duration::from_millis(300)).await;

    alice_client.send_room("test-room", "hello room").await.unwrap();

    let bob_inbox = poll_room_inbox_until(&mut bob_client, "test-room", 1, Duration::from_secs(3)).await;
    assert_eq!(bob_inbox.len(), 1);
    assert_eq!(bob_inbox[0].body, "hello room");
}

#[tokio::test]
async fn secure_room_same_passphrase() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client
        .join_room("secret-room", Some("my-pass"))
        .await
        .unwrap();
    bob_client
        .join_room("secret-room", Some("my-pass"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    alice_client
        .send_room("secret-room", "encrypted msg")
        .await
        .unwrap();

    let bob_inbox =
        poll_room_inbox_until(&mut bob_client, "secret-room", 1, Duration::from_secs(3)).await;
    assert_eq!(bob_inbox.len(), 1);
    assert_eq!(bob_inbox[0].body, "encrypted msg");
}

#[tokio::test]
async fn secure_room_wrong_passphrase() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client
        .join_room("secret-room", Some("pass-a"))
        .await
        .unwrap();
    bob_client
        .join_room("secret-room", Some("pass-b"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    alice_client
        .send_room("secret-room", "cant read this")
        .await
        .unwrap();

    // Wait and verify Bob's room inbox stays empty (decryption fails silently)
    tokio::time::sleep(Duration::from_secs(1)).await;
    let bob_inbox = bob_client.room_inbox("secret-room").await.unwrap();
    assert!(
        bob_inbox.is_empty(),
        "Bob should not decrypt messages with wrong passphrase"
    );
}

#[tokio::test]
async fn room_not_received_after_leave() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client.join_room("test-room", None).await.unwrap();
    bob_client.join_room("test-room", None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify Bob receives messages first
    alice_client.send_room("test-room", "msg before").await.unwrap();
    let bob_inbox =
        poll_room_inbox_until(&mut bob_client, "test-room", 1, Duration::from_secs(3)).await;
    assert_eq!(bob_inbox.len(), 1);

    // Bob leaves
    bob_client.leave_room("test-room").await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Wait for cooldown before Alice sends again
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Alice sends another message
    alice_client.send_room("test-room", "msg after").await.unwrap();

    // Bob's room inbox should not grow
    tokio::time::sleep(Duration::from_secs(1)).await;
    let bob_inbox = bob_client.room_inbox("test-room").await.unwrap_or_default();
    assert!(
        !bob_inbox.iter().any(|m| m.body == "msg after"),
        "Bob should not receive messages after leaving"
    );
}

#[tokio::test]
async fn room_cooldown_enforced() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();

    alice_client.join_room("test-room", None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // First send should succeed
    alice_client.send_room("test-room", "first").await.unwrap();

    // Second immediate send should fail with cooldown error
    let result = alice_client.try_send_room("test-room", "second").await.unwrap();
    match result {
        Response::Error { code, .. } => {
            assert_eq!(code, "cooldown", "expected cooldown error");
        }
        Response::Ok { .. } => {
            panic!("expected cooldown error, got Ok");
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[tokio::test]
async fn room_message_length_limit() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();

    alice_client.join_room("test-room", None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Message over 140 chars should be rejected
    let long_msg = "x".repeat(141);
    let result = alice_client.try_send_room("test-room", &long_msg).await.unwrap();
    match result {
        Response::Error { code, .. } => {
            assert_eq!(code, "message_too_long");
        }
        other => panic!("expected message_too_long error, got {other:?}"),
    }
}

#[tokio::test]
async fn three_nodes_in_room() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let carol = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();
    let mut carol_client = TestClient::connect(&carol.socket_path).await.unwrap();

    alice_client.join_room("group", None).await.unwrap();
    bob_client.join_room("group", None).await.unwrap();
    carol_client.join_room("group", None).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    alice_client.send_room("group", "hello all").await.unwrap();

    let bob_inbox = poll_room_inbox_until(&mut bob_client, "group", 1, Duration::from_secs(3)).await;
    let carol_inbox = poll_room_inbox_until(&mut carol_client, "group", 1, Duration::from_secs(3)).await;

    assert_eq!(bob_inbox.len(), 1);
    assert_eq!(bob_inbox[0].body, "hello all");
    assert_eq!(carol_inbox.len(), 1);
    assert_eq!(carol_inbox[0].body, "hello all");
}

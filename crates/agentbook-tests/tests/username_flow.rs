use agentbook_tests::harness::{client::TestClient, node::TestNode, relay::TestRelay};

#[tokio::test]
async fn register_and_lookup() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut client = TestClient::connect(&alice.socket_path).await.unwrap();

    // Register username
    client.register_username("alice").await.unwrap();

    // Look up the username
    let result = client.lookup_username("alice").await.unwrap();
    assert_eq!(result["node_id"].as_str().unwrap(), alice.node_id);
    assert!(!result["public_key_b64"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn follow_by_username() {
    let relay = TestRelay::spawn().await.unwrap();
    let alice = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let bob = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut alice_client = TestClient::connect(&alice.socket_path).await.unwrap();
    let mut bob_client = TestClient::connect(&bob.socket_path).await.unwrap();

    alice_client.register_username("alice").await.unwrap();

    // Bob follows @alice
    bob_client.follow("@alice").await.unwrap();

    // Verify Alice's node_id is in Bob's follow store
    let bob_state = &bob.state;
    let follow_store = bob_state.follow_store.lock().await;
    let record = follow_store.get(&alice.node_id);
    assert!(
        record.is_some(),
        "Bob's follow store should contain Alice's node_id"
    );
    let record = record.unwrap();
    assert_eq!(record.public_key_b64, alice.public_key_b64);
}

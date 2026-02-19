use agentbook_tests::harness::{client::TestClient, node::TestNode, poll_inbox_until, relay::TestRelay};
use std::time::Duration;

#[tokio::test]
async fn feed_post_delivered_to_follower() {
    let relay = TestRelay::spawn().await.unwrap();
    let poster = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let follower = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut poster_client = TestClient::connect(&poster.socket_path).await.unwrap();
    let mut follower_client = TestClient::connect(&follower.socket_path).await.unwrap();

    // Both register usernames so relay has their pubkeys
    poster_client.register_username("poster").await.unwrap();
    follower_client.register_username("follower").await.unwrap();

    // Follower follows poster via @username (notify_relay_follow records this on the relay)
    follower_client.follow("@poster").await.unwrap();
    // Wait for relay to process follow notification
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Poster posts to feed — fetch_followers_from_relay will return follower's info
    poster_client.post_feed("hello world!").await.unwrap();

    // Poll follower's inbox (feed posts are delivered as individual encrypted envelopes)
    let inbox = poll_inbox_until(&mut follower_client, 1, Duration::from_secs(5)).await;
    assert!(
        inbox.iter().any(|m| m.body == "hello world!"),
        "follower should receive the feed post, got: {:?}",
        inbox.iter().map(|m| &m.body).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn feed_post_not_delivered_to_non_follower() {
    let relay = TestRelay::spawn().await.unwrap();
    let poster = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let follower = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let non_follower = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut poster_client = TestClient::connect(&poster.socket_path).await.unwrap();
    let mut follower_client = TestClient::connect(&follower.socket_path).await.unwrap();
    let mut non_follower_client = TestClient::connect(&non_follower.socket_path).await.unwrap();

    poster_client.register_username("poster").await.unwrap();
    follower_client.register_username("follower").await.unwrap();
    non_follower_client
        .register_username("lurker")
        .await
        .unwrap();

    follower_client.follow("@poster").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    poster_client.post_feed("secret post").await.unwrap();

    // Follower gets the post
    let inbox = poll_inbox_until(&mut follower_client, 1, Duration::from_secs(5)).await;
    assert!(inbox.iter().any(|m| m.body == "secret post"));

    // Non-follower does NOT get the post (feed is only sent to followers)
    tokio::time::sleep(Duration::from_secs(1)).await;
    let nf_inbox = non_follower_client.inbox().await.unwrap();
    assert!(
        nf_inbox.is_empty(),
        "non-follower should not receive feed post"
    );
}

#[tokio::test]
async fn feed_post_to_multiple_followers() {
    let relay = TestRelay::spawn().await.unwrap();
    let poster = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let follower_a = TestNode::spawn(&relay.relay_addr()).await.unwrap();
    let follower_b = TestNode::spawn(&relay.relay_addr()).await.unwrap();

    let mut poster_client = TestClient::connect(&poster.socket_path).await.unwrap();
    let mut fa_client = TestClient::connect(&follower_a.socket_path).await.unwrap();
    let mut fb_client = TestClient::connect(&follower_b.socket_path).await.unwrap();

    poster_client.register_username("poster").await.unwrap();
    fa_client.register_username("followa").await.unwrap();
    fb_client.register_username("followb").await.unwrap();

    fa_client.follow("@poster").await.unwrap();
    fb_client.follow("@poster").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    poster_client.post_feed("broadcast msg").await.unwrap();

    let inbox_a = poll_inbox_until(&mut fa_client, 1, Duration::from_secs(5)).await;
    let inbox_b = poll_inbox_until(&mut fb_client, 1, Duration::from_secs(5)).await;

    assert!(
        inbox_a.iter().any(|m| m.body == "broadcast msg"),
        "follower A should receive, got: {:?}",
        inbox_a.iter().map(|m| &m.body).collect::<Vec<_>>()
    );
    assert!(
        inbox_b.iter().any(|m| m.body == "broadcast msg"),
        "follower B should receive, got: {:?}",
        inbox_b.iter().map(|m| &m.body).collect::<Vec<_>>()
    );
}

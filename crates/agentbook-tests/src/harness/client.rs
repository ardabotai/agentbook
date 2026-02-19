use agentbook::client::NodeClient;
use agentbook::protocol::{InboxEntry, Request, Response, RoomInfo};
use anyhow::{Result, bail};
use std::path::Path;

/// Convenience wrapper over `NodeClient` for integration tests.
pub struct TestClient {
    inner: NodeClient,
}

impl TestClient {
    /// Connect to a node daemon at the given socket path.
    pub async fn connect(socket_path: &Path) -> Result<Self> {
        let inner = NodeClient::connect(socket_path).await?;
        Ok(Self { inner })
    }

    /// Get identity info as raw JSON.
    pub async fn identity(&mut self) -> Result<serde_json::Value> {
        match self.inner.request(Request::Identity).await? {
            Some(data) => Ok(data),
            None => bail!("identity returned no data"),
        }
    }

    /// Follow a target (node_id or @username).
    pub async fn follow(&mut self, target: &str) -> Result<()> {
        self.inner
            .request(Request::Follow {
                target: target.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Unfollow a target.
    pub async fn unfollow(&mut self, target: &str) -> Result<()> {
        self.inner
            .request(Request::Unfollow {
                target: target.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Block a target.
    pub async fn block(&mut self, target: &str) -> Result<()> {
        self.inner
            .request(Request::Block {
                target: target.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Send a DM.
    pub async fn send_dm(&mut self, to: &str, body: &str) -> Result<()> {
        self.inner
            .request(Request::SendDm {
                to: to.to_string(),
                body: body.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Send a DM, returning the raw response (including errors).
    pub async fn try_send_dm(&mut self, to: &str, body: &str) -> Result<Response> {
        self.inner
            .send(Request::SendDm {
                to: to.to_string(),
                body: body.to_string(),
            })
            .await?;
        loop {
            match self.inner.next_response().await? {
                Response::Event { .. } | Response::Hello { .. } => continue,
                resp => return Ok(resp),
            }
        }
    }

    /// Post to feed.
    pub async fn post_feed(&mut self, body: &str) -> Result<Option<serde_json::Value>> {
        self.inner
            .request(Request::PostFeed {
                body: body.to_string(),
            })
            .await
    }

    /// Post to feed, returning the raw response (including errors).
    pub async fn try_post_feed(&mut self, body: &str) -> Result<Response> {
        self.inner
            .send(Request::PostFeed {
                body: body.to_string(),
            })
            .await?;
        loop {
            match self.inner.next_response().await? {
                Response::Event { .. } | Response::Hello { .. } => continue,
                resp => return Ok(resp),
            }
        }
    }

    /// Get inbox messages.
    pub async fn inbox(&mut self) -> Result<Vec<InboxEntry>> {
        match self.inner.request(Request::Inbox {
            unread_only: false,
            limit: None,
        }).await? {
            Some(data) => Ok(serde_json::from_value(data)?),
            None => Ok(vec![]),
        }
    }

    /// Register a username on the relay.
    pub async fn register_username(&mut self, name: &str) -> Result<()> {
        self.inner
            .request(Request::RegisterUsername {
                username: name.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Look up a username on the relay.
    pub async fn lookup_username(&mut self, name: &str) -> Result<serde_json::Value> {
        match self.inner.request(Request::LookupUsername {
            username: name.to_string(),
        }).await? {
            Some(data) => Ok(data),
            None => bail!("lookup returned no data"),
        }
    }

    /// Join a room.
    pub async fn join_room(&mut self, room: &str, passphrase: Option<&str>) -> Result<()> {
        self.inner
            .request(Request::JoinRoom {
                room: room.to_string(),
                passphrase: passphrase.map(|s| s.to_string()),
            })
            .await?;
        Ok(())
    }

    /// Leave a room.
    pub async fn leave_room(&mut self, room: &str) -> Result<()> {
        self.inner
            .request(Request::LeaveRoom {
                room: room.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Send a message to a room.
    pub async fn send_room(&mut self, room: &str, body: &str) -> Result<()> {
        self.inner
            .request(Request::SendRoom {
                room: room.to_string(),
                body: body.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Send a room message, returning the raw response (including errors).
    pub async fn try_send_room(&mut self, room: &str, body: &str) -> Result<Response> {
        self.inner
            .send(Request::SendRoom {
                room: room.to_string(),
                body: body.to_string(),
            })
            .await?;
        loop {
            match self.inner.next_response().await? {
                Response::Event { .. } | Response::Hello { .. } => continue,
                resp => return Ok(resp),
            }
        }
    }

    /// Get room inbox messages.
    pub async fn room_inbox(&mut self, room: &str) -> Result<Vec<InboxEntry>> {
        match self.inner.request(Request::RoomInbox {
            room: room.to_string(),
            limit: None,
        }).await? {
            Some(data) => Ok(serde_json::from_value(data)?),
            None => Ok(vec![]),
        }
    }

    /// List joined rooms.
    pub async fn list_rooms(&mut self) -> Result<Vec<RoomInfo>> {
        match self.inner.request(Request::ListRooms).await? {
            Some(data) => Ok(serde_json::from_value(data)?),
            None => Ok(vec![]),
        }
    }
}

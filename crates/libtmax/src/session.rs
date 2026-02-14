use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use tokio::sync::broadcast;
use tracing::{debug, info};

use tmax_protocol::{AttachMode, Event, SandboxConfig, SessionId};

use crate::broker::EventBroker;
use crate::error::TmaxError;
use crate::output::{LiveBuffer, Marker};

const DEFAULT_BUFFER_SIZE: usize = 10_000;

/// Configuration for creating a new session.
pub struct SessionCreateConfig {
    pub exec: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub label: Option<String>,
    pub sandbox: Option<SandboxConfig>,
    pub parent_id: Option<SessionId>,
    pub cols: u16,
    pub rows: u16,
}

/// Metadata about a session.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub label: Option<String>,
    pub exec: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub sandbox: Option<SandboxConfig>,
    pub parent_id: Option<SessionId>,
    pub created_at: SystemTime,
}

/// An attachment to a session (edit or view).
#[derive(Debug, Clone)]
pub struct Attachment {
    pub id: String,
    pub mode: AttachMode,
}

/// Exit status of a completed session.
#[derive(Debug, Clone)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

/// A single terminal session with its PTY, output buffer, and metadata.
pub struct Session {
    pub id: SessionId,
    pub metadata: SessionMetadata,
    pub live_buffer: LiveBuffer,
    pub markers: Vec<Marker>,
    pub attachments: Vec<Attachment>,
    pub exit_status: Option<ExitStatus>,
    master_pty: Box<dyn MasterPty + Send>,
    /// Writer handle for sending input to the PTY.
    /// Wrapped in Option so we can take() it for the I/O loop.
    pty_writer: Option<Box<dyn std::io::Write + Send>>,
    /// Child process handle, wrapped in Option so we can take() it for exit code capture.
    child: Option<Box<dyn portable_pty::Child + Send>>,
}

impl Session {
    pub fn has_edit_attachment(&self) -> bool {
        self.attachments.iter().any(|a| a.mode == AttachMode::Edit)
    }

    pub fn attachment_count(&self) -> usize {
        self.attachments.len()
    }

    pub fn edit_attachment_count(&self) -> usize {
        self.attachments
            .iter()
            .filter(|a| a.mode == AttachMode::Edit)
            .count()
    }

    pub fn to_info(&self, children: Vec<SessionId>) -> tmax_protocol::SessionInfo {
        tmax_protocol::SessionInfo {
            id: self.id.clone(),
            label: self.metadata.label.clone(),
            exec: self.metadata.exec.clone(),
            args: self.metadata.args.clone(),
            cwd: self.metadata.cwd.clone(),
            parent_id: self.metadata.parent_id.clone(),
            children,
            sandbox: self.metadata.sandbox.clone(),
            created_at_epoch_ms: self
                .metadata
                .created_at
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            attachment_count: self.attachment_count(),
            edit_attachment_count: self.edit_attachment_count(),
            exited: self.exit_status.is_some(),
            exit_code: self.exit_status.as_ref().and_then(|e| e.code),
        }
    }
}

/// Manages all sessions and their lifecycle.
pub struct SessionManager {
    sessions: HashMap<SessionId, Session>,
    session_tree: HashMap<SessionId, Vec<SessionId>>, // parent -> children
    broker: EventBroker,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            session_tree: HashMap::new(),
            broker: EventBroker::new(),
        }
    }

    /// Create a new session, spawning a PTY process.
    /// Returns the session ID and a broadcast receiver for the PTY I/O task.
    pub fn create_session(
        &mut self,
        config: SessionCreateConfig,
    ) -> Result<(SessionId, broadcast::Receiver<Event>), TmaxError> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let cwd = config
            .cwd
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));

        // Validate parent exists if specified
        if let Some(ref parent_id) = config.parent_id {
            if !self.sessions.contains_key(parent_id) {
                return Err(TmaxError::SessionNotFound(parent_id.clone()));
            }
        }

        // Spawn PTY
        let pty_system = native_pty_system();
        let pty_pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TmaxError::PtyError(e.to_string()))?;

        let mut cmd = CommandBuilder::new(&config.exec);
        cmd.args(&config.args);
        cmd.cwd(&cwd);

        let child = pty_pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| TmaxError::PtyError(e.to_string()))?;

        // Drop slave - we only need the master side
        drop(pty_pair.slave);

        // Get writer for input
        let writer = pty_pair
            .master
            .take_writer()
            .map_err(|e| TmaxError::PtyError(e.to_string()))?;

        // Create event channel
        let event_tx = self.broker.create_channel(&session_id);
        let event_rx = event_tx.subscribe();

        let metadata = SessionMetadata {
            label: config.label.clone(),
            exec: config.exec,
            args: config.args,
            cwd,
            sandbox: config.sandbox,
            parent_id: config.parent_id.clone(),
            created_at: SystemTime::now(),
        };

        let session = Session {
            id: session_id.clone(),
            metadata,
            live_buffer: LiveBuffer::new(DEFAULT_BUFFER_SIZE),
            markers: Vec::new(),
            attachments: Vec::new(),
            exit_status: None,
            master_pty: pty_pair.master,
            pty_writer: Some(writer),
            child: Some(child),
        };

        // Track in parent-child tree
        if let Some(ref parent_id) = config.parent_id {
            self.session_tree
                .entry(parent_id.clone())
                .or_default()
                .push(session_id.clone());
        }
        self.session_tree
            .entry(session_id.clone())
            .or_default();

        info!(session_id = %session_id, label = ?config.label, "session created");

        // Broadcast creation event
        self.broker.broadcast(
            &session_id,
            Event::SessionCreated {
                session_id: session_id.clone(),
                label: config.label,
            },
        );

        self.sessions.insert(session_id.clone(), session);
        Ok((session_id, event_rx))
    }

    /// Take the PTY reader from a session for use in the I/O loop.
    /// This can only be called once per session.
    pub fn take_pty_reader(
        &mut self,
        session_id: &SessionId,
    ) -> Result<Box<dyn std::io::Read + Send>, TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        session
            .master_pty
            .try_clone_reader()
            .map_err(|e| TmaxError::PtyError(e.to_string()))
    }

    /// Take the child process handle from a session for exit code capture.
    /// This can only be called once per session.
    pub fn take_child(
        &mut self,
        session_id: &SessionId,
    ) -> Result<Box<dyn portable_pty::Child + Send>, TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        session
            .child
            .take()
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))
    }

    /// Record output from the PTY I/O loop into the session's live buffer
    /// and broadcast the event.
    pub fn record_output(
        &mut self,
        session_id: &SessionId,
        data: Vec<u8>,
    ) -> Result<u64, TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        let seq = session.live_buffer.push(data.clone());

        self.broker.broadcast(
            session_id,
            Event::Output {
                session_id: session_id.clone(),
                seq,
                data,
            },
        );

        Ok(seq)
    }

    /// Record that a session's process has exited.
    pub fn record_exit(
        &mut self,
        session_id: &SessionId,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> Result<(), TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        session.exit_status = Some(ExitStatus {
            code: exit_code,
            signal,
        });

        self.broker.broadcast(
            session_id,
            Event::SessionExited {
                session_id: session_id.clone(),
                exit_code,
                signal,
            },
        );

        info!(session_id = %session_id, exit_code = ?exit_code, "session exited");
        Ok(())
    }

    /// Destroy a session, optionally cascading to children.
    pub fn destroy_session(
        &mut self,
        session_id: &SessionId,
        cascade: bool,
    ) -> Result<Vec<SessionId>, TmaxError> {
        if !self.sessions.contains_key(session_id) {
            return Err(TmaxError::SessionNotFound(session_id.clone()));
        }

        let mut destroyed = Vec::new();

        if cascade {
            // Collect children first to avoid borrow issues
            let children = self
                .session_tree
                .get(session_id)
                .cloned()
                .unwrap_or_default();

            for child_id in children {
                if let Ok(mut child_destroyed) = self.destroy_session(&child_id, true) {
                    destroyed.append(&mut child_destroyed);
                }
            }
        }

        // Remove from parent's children list
        if let Some(parent_id) = self
            .sessions
            .get(session_id)
            .and_then(|s| s.metadata.parent_id.clone())
        {
            if let Some(children) = self.session_tree.get_mut(&parent_id) {
                children.retain(|id| id != session_id);
            }
        }

        // Remove session
        self.sessions.remove(session_id);
        self.session_tree.remove(session_id);

        // Broadcast destroy event and cleanup channel
        self.broker.broadcast(
            session_id,
            Event::SessionDestroyed {
                session_id: session_id.clone(),
            },
        );
        self.broker.remove_channel(session_id);

        destroyed.push(session_id.clone());
        info!(session_id = %session_id, "session destroyed");
        Ok(destroyed)
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> Vec<tmax_protocol::SessionInfo> {
        self.sessions
            .values()
            .map(|s| {
                let children = self
                    .session_tree
                    .get(&s.id)
                    .cloned()
                    .unwrap_or_default();
                s.to_info(children)
            })
            .collect()
    }

    /// Get info about a specific session.
    pub fn get_session_info(
        &self,
        session_id: &SessionId,
    ) -> Result<tmax_protocol::SessionInfo, TmaxError> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        let children = self
            .session_tree
            .get(session_id)
            .cloned()
            .unwrap_or_default();

        Ok(session.to_info(children))
    }

    /// Build a session tree for hierarchical display.
    pub fn session_tree(&self) -> Vec<tmax_protocol::SessionTreeNode> {
        // Find root sessions (no parent)
        let roots: Vec<&Session> = self
            .sessions
            .values()
            .filter(|s| s.metadata.parent_id.is_none())
            .collect();

        roots.iter().map(|s| self.build_tree_node(s)).collect()
    }

    fn build_tree_node(&self, session: &Session) -> tmax_protocol::SessionTreeNode {
        let children_ids = self
            .session_tree
            .get(&session.id)
            .cloned()
            .unwrap_or_default();

        let children: Vec<tmax_protocol::SessionTreeNode> = children_ids
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .map(|s| self.build_tree_node(s))
            .collect();

        tmax_protocol::SessionTreeNode {
            info: session.to_info(children_ids),
            children,
        }
    }

    /// Attach to a session in the given mode.
    pub fn attach(
        &mut self,
        session_id: &SessionId,
        mode: AttachMode,
    ) -> Result<String, TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        // Only one edit attachment allowed at a time
        if mode == AttachMode::Edit && session.has_edit_attachment() {
            return Err(TmaxError::AttachmentDenied(
                "session already has an edit attachment".to_string(),
            ));
        }

        let attachment_id = uuid::Uuid::new_v4().to_string();
        session.attachments.push(Attachment {
            id: attachment_id.clone(),
            mode,
        });

        self.broker.broadcast(
            session_id,
            Event::Attached {
                session_id: session_id.clone(),
                mode,
                attachment_id: attachment_id.clone(),
            },
        );

        debug!(session_id = %session_id, attachment_id = %attachment_id, mode = ?mode, "attached");
        Ok(attachment_id)
    }

    /// Detach from a session by attachment ID.
    pub fn detach(
        &mut self,
        session_id: &SessionId,
        attachment_id: &str,
    ) -> Result<(), TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        let before_len = session.attachments.len();
        session.attachments.retain(|a| a.id != attachment_id);

        if session.attachments.len() == before_len {
            return Err(TmaxError::AttachmentDenied(format!(
                "attachment {attachment_id} not found"
            )));
        }

        self.broker.broadcast(
            session_id,
            Event::Detached {
                session_id: session_id.clone(),
                attachment_id: attachment_id.to_string(),
            },
        );

        debug!(session_id = %session_id, attachment_id = %attachment_id, "detached");
        Ok(())
    }

    /// Send input to a session's PTY. Requires an edit attachment.
    pub fn send_input(
        &mut self,
        session_id: &SessionId,
        data: &[u8],
    ) -> Result<(), TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        if session.exit_status.is_some() {
            return Err(TmaxError::SessionExited(session_id.clone()));
        }

        // Write to PTY
        if let Some(ref mut writer) = session.pty_writer {
            use std::io::Write;
            writer
                .write_all(data)
                .map_err(|e| TmaxError::PtyError(e.to_string()))?;
            writer
                .flush()
                .map_err(|e| TmaxError::PtyError(e.to_string()))?;
        } else {
            return Err(TmaxError::PtyError("no PTY writer available".to_string()));
        }

        Ok(())
    }

    /// Resize a session's PTY.
    pub fn resize(
        &mut self,
        session_id: &SessionId,
        cols: u16,
        rows: u16,
    ) -> Result<(), TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        session
            .master_pty
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| TmaxError::PtyError(e.to_string()))?;

        Ok(())
    }

    /// Insert a marker at the current output position.
    pub fn insert_marker(
        &mut self,
        session_id: &SessionId,
        name: String,
    ) -> Result<u64, TmaxError> {
        let session = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        let seq = session.live_buffer.next_seq();
        let marker = Marker {
            name: name.clone(),
            seq,
            timestamp: SystemTime::now(),
        };
        session.markers.push(marker);

        self.broker.broadcast(
            session_id,
            Event::MarkerInserted {
                session_id: session_id.clone(),
                name,
                seq,
            },
        );

        Ok(seq)
    }

    /// List markers for a session.
    pub fn list_markers(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<tmax_protocol::MarkerInfo>, TmaxError> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        Ok(session
            .markers
            .iter()
            .map(|m| tmax_protocol::MarkerInfo {
                name: m.name.clone(),
                seq: m.seq,
                timestamp_epoch_ms: m
                    .timestamp
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
            })
            .collect())
    }

    /// Subscribe to a session's event stream.
    pub fn subscribe(
        &self,
        session_id: &SessionId,
    ) -> Result<broadcast::Receiver<Event>, TmaxError> {
        self.broker
            .subscribe(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))
    }

    /// Get catch-up chunks for a reconnecting subscriber.
    pub fn get_catchup(
        &self,
        session_id: &SessionId,
        last_seq: Option<u64>,
    ) -> Result<Option<Vec<crate::output::OutputChunk>>, TmaxError> {
        let session = self
            .sessions
            .get(session_id)
            .ok_or_else(|| TmaxError::SessionNotFound(session_id.clone()))?;

        Ok(crate::broker::compute_catchup(&session.live_buffer, last_seq))
    }

    /// Check if a session exists.
    pub fn session_exists(&self, session_id: &SessionId) -> bool {
        self.sessions.contains_key(session_id)
    }

}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_echo_config() -> SessionCreateConfig {
        SessionCreateConfig {
            exec: "echo".to_string(),
            args: vec!["hello".to_string()],
            cwd: None,
            label: Some("test".to_string()),
            sandbox: None,
            parent_id: None,
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn create_and_list_session() {
        let mut mgr = SessionManager::new();
        let (id, _rx) = mgr.create_session(create_echo_config()).unwrap();

        assert!(mgr.session_exists(&id));
        assert_eq!(mgr.list_sessions().len(), 1);

        let sessions = mgr.list_sessions();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].exec, "echo");
        assert_eq!(sessions[0].label, Some("test".to_string()));
    }

    #[test]
    fn session_info() {
        let mut mgr = SessionManager::new();
        let (id, _rx) = mgr.create_session(create_echo_config()).unwrap();

        let info = mgr.get_session_info(&id).unwrap();
        assert_eq!(info.exec, "echo");
        assert!(!info.exited);
    }

    #[test]
    fn destroy_session() {
        let mut mgr = SessionManager::new();
        let (id, _rx) = mgr.create_session(create_echo_config()).unwrap();

        let destroyed = mgr.destroy_session(&id, false).unwrap();
        assert_eq!(destroyed, vec![id.clone()]);
        assert!(!mgr.session_exists(&id));
    }

    #[test]
    fn session_not_found() {
        let mgr = SessionManager::new();
        let result = mgr.get_session_info(&"nonexistent".to_string());
        assert!(result.is_err());
    }

    #[test]
    fn attach_and_detach() {
        let mut mgr = SessionManager::new();
        let (id, _rx) = mgr.create_session(create_echo_config()).unwrap();

        let att_id = mgr.attach(&id, AttachMode::Edit).unwrap();
        let info = mgr.get_session_info(&id).unwrap();
        assert_eq!(info.attachment_count, 1);
        assert_eq!(info.edit_attachment_count, 1);

        // Second edit should fail
        let result = mgr.attach(&id, AttachMode::Edit);
        assert!(result.is_err());

        // View should work
        let _view_id = mgr.attach(&id, AttachMode::View).unwrap();
        let info = mgr.get_session_info(&id).unwrap();
        assert_eq!(info.attachment_count, 2);

        mgr.detach(&id, &att_id).unwrap();
        let info = mgr.get_session_info(&id).unwrap();
        assert_eq!(info.edit_attachment_count, 0);

        // Now another edit should work
        let _new_edit = mgr.attach(&id, AttachMode::Edit).unwrap();
    }

    #[test]
    fn markers() {
        let mut mgr = SessionManager::new();
        let (id, _rx) = mgr.create_session(create_echo_config()).unwrap();

        let seq = mgr.insert_marker(&id, "start".to_string()).unwrap();
        assert_eq!(seq, 0);

        mgr.record_output(&id, b"some output".to_vec()).unwrap();

        let seq2 = mgr.insert_marker(&id, "after-output".to_string()).unwrap();
        assert_eq!(seq2, 1);

        let markers = mgr.list_markers(&id).unwrap();
        assert_eq!(markers.len(), 2);
        assert_eq!(markers[0].name, "start");
        assert_eq!(markers[1].name, "after-output");
    }

    #[test]
    fn session_nesting() {
        let mut mgr = SessionManager::new();
        let (parent_id, _rx1) = mgr.create_session(create_echo_config()).unwrap();

        let child_config = SessionCreateConfig {
            parent_id: Some(parent_id.clone()),
            ..create_echo_config()
        };
        let (child_id, _rx2) = mgr.create_session(child_config).unwrap();

        let parent_info = mgr.get_session_info(&parent_id).unwrap();
        assert_eq!(parent_info.children.len(), 1);
        assert_eq!(parent_info.children[0], child_id);

        let child_info = mgr.get_session_info(&child_id).unwrap();
        assert_eq!(child_info.parent_id, Some(parent_id.clone()));

        // Tree should show hierarchy
        let tree = mgr.session_tree();
        assert_eq!(tree.len(), 1); // one root
        assert_eq!(tree[0].children.len(), 1);
    }

    #[test]
    fn cascade_destroy() {
        let mut mgr = SessionManager::new();
        let (parent_id, _rx1) = mgr.create_session(create_echo_config()).unwrap();

        let child_config = SessionCreateConfig {
            parent_id: Some(parent_id.clone()),
            ..create_echo_config()
        };
        let (child_id, _rx2) = mgr.create_session(child_config).unwrap();

        let destroyed = mgr.destroy_session(&parent_id, true).unwrap();
        assert_eq!(destroyed.len(), 2);
        assert!(!mgr.session_exists(&parent_id));
        assert!(!mgr.session_exists(&child_id));
    }

    #[test]
    fn record_output_and_catchup() {
        let mut mgr = SessionManager::new();
        let (id, _rx) = mgr.create_session(create_echo_config()).unwrap();

        mgr.record_output(&id, b"line 1\n".to_vec()).unwrap();
        mgr.record_output(&id, b"line 2\n".to_vec()).unwrap();

        // Full catchup
        let chunks = mgr.get_catchup(&id, None).unwrap().unwrap();
        assert_eq!(chunks.len(), 2);

        // Catchup from seq 0
        let chunks = mgr.get_catchup(&id, Some(0)).unwrap().unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].data, b"line 2\n");
    }
}

use anyhow::{Context, Result, bail};
use base64::Engine;
use futures_util::StreamExt;
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;
use tmax_protocol::{AttachMode, Event, Request, Response};
use tokio::time::{Instant, sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

struct WebProcess {
    child: Child,
}

impl WebProcess {
    fn spawn(socket_path: &str, listen: &str) -> Result<Self> {
        let child = Command::new(assert_cmd::cargo::cargo_bin!("tmax-web"))
            .arg("--socket")
            .arg(socket_path)
            .arg("--listen")
            .arg(listen)
            .arg("--batch-ms")
            .arg("5")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn tmax-web")?;

        Ok(Self { child })
    }

    async fn wait_ready(&mut self, addr: &str) -> Result<()> {
        let start = Instant::now();
        loop {
            if self.child.try_wait()?.is_some() {
                bail!("tmax-web exited before becoming ready");
            }
            if TcpStream::connect(addr).is_ok() {
                return Ok(());
            }
            if start.elapsed() > Duration::from_secs(5) {
                bail!("timed out waiting for tmax-web listener");
            }
            sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for WebProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn reserve_local_addr() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0").context("failed to bind probe listener")?;
    let addr = listener.local_addr().context("failed to read probe addr")?;
    drop(listener);
    Ok(addr.to_string())
}

fn mock_backend(
    socket_path: PathBuf,
    expect_last_seq: Option<u64>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || {
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }
        let listener =
            UnixListener::bind(&socket_path).with_context(|| "failed to bind mock backend")?;
        let (stream, _) = listener
            .accept()
            .context("failed to accept web connection")?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .context("failed to set backend timeout")?;

        let read_half = stream
            .try_clone()
            .context("failed to clone backend stream")?;
        let mut reader = BufReader::new(read_half);
        let mut writer = BufWriter::new(stream);

        writeln!(
            writer,
            "{}",
            serde_json::to_string(&Response::hello(vec!["mock".to_string()]))?
        )?;
        writer.flush()?;

        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("web disconnected before attach request");
        }
        let req: Request = serde_json::from_str(line.trim_end())?;
        match req {
            Request::Attach {
                session_id,
                mode,
                last_seq_seen,
            } => {
                if session_id != "s1" {
                    bail!("unexpected session_id: {session_id}");
                }
                if mode != AttachMode::View {
                    bail!("expected view attach, got {mode:?}");
                }
                if last_seq_seen != expect_last_seq {
                    bail!("unexpected last_seq: {last_seq_seen:?}");
                }
            }
            other => bail!("expected attach request, got {other:?}"),
        }

        writeln!(
            writer,
            "{}",
            json!({
                "type": "ok",
                "data": {
                    "attachment": {
                        "attachment_id": "a1",
                        "session_id": "s1",
                        "mode": "view",
                        "created_at_ms": 1
                    }
                }
            })
        )?;

        let payload = if expect_last_seq.is_some() {
            b"catchup".to_vec()
        } else {
            b"frame_one|frame_two".to_vec()
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
        let event = Response::Event {
            event: Box::new(Event::Output {
                session_id: "s1".to_string(),
                seq: 42,
                data_b64: encoded,
            }),
        };
        writeln!(writer, "{}", serde_json::to_string(&event)?)?;
        writer.flush()?;

        line.clear();
        let _ = reader.read_line(&mut line);

        Ok(())
    })
}

#[tokio::test]
async fn ws_integration_streams_ordered_output_frames() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = temp.path().join("backend.sock");
    let backend = mock_backend(socket_path.clone(), None);

    let listen = reserve_local_addr()?;
    let mut web = WebProcess::spawn(
        socket_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid socket path"))?,
        &listen,
    )?;
    web.wait_ready(&listen).await?;

    let (mut ws, _) = connect_async(format!("ws://{listen}/ws/session/s1?mode=view")).await?;
    let bytes = timeout(Duration::from_secs(5), async {
        loop {
            let Some(msg) = ws.next().await else {
                bail!("websocket closed before output");
            };
            if let Message::Binary(data) = msg? {
                return Ok::<Vec<u8>, anyhow::Error>(data.to_vec());
            }
        }
    })
    .await
    .context("timed out waiting for websocket binary frame")??;

    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("frame_one|frame_two"),
        "unexpected websocket payload: {text}"
    );

    let _ = ws.close(None).await;
    backend.join().expect("mock backend thread panicked")?;
    Ok(())
}

#[tokio::test]
async fn ws_integration_reconnect_forwards_last_seq_for_catch_up() -> Result<()> {
    let temp = tempfile::tempdir().context("failed to create tempdir")?;
    let socket_path = temp.path().join("backend.sock");
    let backend = mock_backend(socket_path.clone(), Some(41));

    let listen = reserve_local_addr()?;
    let mut web = WebProcess::spawn(
        socket_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid socket path"))?,
        &listen,
    )?;
    web.wait_ready(&listen).await?;

    let (mut ws, _) =
        connect_async(format!("ws://{listen}/ws/session/s1?mode=view&last_seq=41")).await?;

    let bytes = timeout(Duration::from_secs(5), async {
        loop {
            let Some(msg) = ws.next().await else {
                bail!("websocket closed before catch-up");
            };
            if let Message::Binary(data) = msg? {
                return Ok::<Vec<u8>, anyhow::Error>(data.to_vec());
            }
        }
    })
    .await
    .context("timed out waiting for reconnect catch-up frame")??;

    assert_eq!(String::from_utf8_lossy(&bytes), "catchup");

    let _ = ws.close(None).await;
    backend.join().expect("mock backend thread panicked")?;
    Ok(())
}

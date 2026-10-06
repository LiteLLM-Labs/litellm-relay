use std::{future::Future, path::PathBuf, pin::Pin, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{
        unix::{OwnedReadHalf, OwnedWriteHalf},
        UnixListener, UnixStream,
    },
    time::timeout,
};

use super::service::{Answer, Asker, McpRefusal, McpRequest, McpService, Question};
pub use crate::broker::socket::bind;
use crate::{
    broker::socket::{admit, identity},
    config::relay_home,
};

pub const SOCKET_FILE: &str = "mcp.sock";
pub const MAX_LINE_BYTES: u64 = 4 * 1024 * 1024;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub fn socket_path() -> PathBuf {
    relay_home().join(SOCKET_FILE)
}

#[derive(Deserialize)]
struct AnswerLine {
    answer: Answer,
}

enum PeerLine {
    Text(String),
    TooLong,
    Closed,
    Unreadable(String),
}

struct PeerLines {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl PeerLines {
    async fn read(&mut self) -> PeerLine {
        let mut line = Vec::new();
        let mut capped = (&mut self.reader).take(MAX_LINE_BYTES + 1);
        match capped.read_until(b'\n', &mut line).await {
            Err(error) => PeerLine::Unreadable(error.to_string()),
            Ok(0) => PeerLine::Closed,
            Ok(_) if line.len() as u64 > MAX_LINE_BYTES && !line.ends_with(b"\n") => {
                PeerLine::TooLong
            }
            Ok(_) => match String::from_utf8(line) {
                Ok(text) => PeerLine::Text(text),
                Err(_) => PeerLine::Unreadable("the line is not UTF-8".to_string()),
            },
        }
    }

    async fn write(&mut self, line: &Value) -> std::io::Result<()> {
        self.writer.write_all(format!("{line}\n").as_bytes()).await
    }

    async fn request(&mut self) -> Result<McpRequest, McpRefusal> {
        let line = match timeout(REQUEST_READ_TIMEOUT, self.read()).await {
            Err(_) => {
                return Err(McpRefusal::BadRequest(
                    "no request line arrived in time".to_string(),
                ))
            }
            Ok(line) => line,
        };
        match line {
            PeerLine::Text(text) => serde_json::from_str(text.trim()).map_err(|error| {
                McpRefusal::BadRequest(format!(
                    "expected one JSON line such as {{\"op\":\"search_tools\",\"query\":\"issues\"}}: {error}"
                ))
            }),
            PeerLine::TooLong => Err(McpRefusal::RequestTooLarge(format!(
                "the request line is longer than the 4 MiB cap ({MAX_LINE_BYTES} bytes)"
            ))),
            PeerLine::Closed => Err(McpRefusal::BadRequest(
                "the connection closed before a request line arrived".to_string(),
            )),
            PeerLine::Unreadable(error) => Err(McpRefusal::BadRequest(format!(
                "the request could not be read: {error}"
            ))),
        }
    }
}

impl Asker for PeerLines {
    fn ask<'a>(
        &'a mut self,
        question: Question,
    ) -> Pin<Box<dyn Future<Output = Answer> + Send + 'a>> {
        Box::pin(async move {
            if self.write(&json!({ "ask": question })).await.is_err() {
                return Answer::Cancel;
            }
            match self.read().await {
                PeerLine::Text(text) => serde_json::from_str::<AnswerLine>(text.trim())
                    .map(|line| line.answer)
                    .unwrap_or(Answer::Cancel),
                PeerLine::TooLong | PeerLine::Closed | PeerLine::Unreadable(_) => Answer::Cancel,
            }
        })
    }
}

pub async fn serve(
    service: Arc<McpService>,
    listener: UnixListener,
    daemon_uid: u32,
) -> Result<()> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("the MCP socket stopped accepting connections")?;
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            let _ = handle(service, stream, daemon_uid).await;
        });
    }
}

async fn handle(service: Arc<McpService>, stream: UnixStream, daemon_uid: u32) -> Result<()> {
    let admitted = admit(identity(&stream), daemon_uid);
    let (reader, writer) = stream.into_split();
    let mut lines = PeerLines {
        reader: BufReader::new(reader),
        writer,
    };
    let outcome = match admitted {
        Err(refusal) => Err(McpRefusal::Broker(refusal)),
        Ok(peer) => match lines.request().await {
            Err(refusal) => Err(refusal),
            Ok(request) => service.handle(peer, request, &mut lines).await,
        },
    };
    let reply = match outcome {
        Ok(result) => json!({ "ok": true, "result": result }),
        Err(refusal) => json!({
            "ok": false,
            "reason": refusal.reason(),
            "message": refusal.message(),
        }),
    };
    lines.write(&reply).await?;
    lines.writer.shutdown().await?;
    let _ = timeout(
        REQUEST_READ_TIMEOUT,
        tokio::io::copy(&mut lines.reader, &mut tokio::io::sink()),
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        broker::{
            socket::daemon_uid,
            test_support::{static_key_settings, Rig},
        },
        mcp::{
            catalog::tests::tool,
            test_support::{service_on, FakeUpstream},
        },
    };
    use std::env;
    use tokio::task::JoinHandle;
    use uuid::Uuid;

    struct Client {
        lines: PeerLines,
    }

    impl Client {
        async fn connect(path: &PathBuf) -> Self {
            let (reader, writer) = UnixStream::connect(path)
                .await
                .expect("connect")
                .into_split();
            Self {
                lines: PeerLines {
                    reader: BufReader::new(reader),
                    writer,
                },
            }
        }

        async fn send(&mut self, line: Value) {
            self.lines.write(&line).await.expect("write");
        }

        async fn receive(&mut self) -> Value {
            match self.lines.read().await {
                PeerLine::Text(text) => serde_json::from_str(text.trim()).expect("JSON line"),
                _ => panic!("the daemon sent no line"),
            }
        }
    }

    async fn exchange(path: &PathBuf, request: Value) -> Value {
        let mut client = Client::connect(path).await;
        client.send(request).await;
        client.receive().await
    }

    fn start(uid: u32) -> (Rig, FakeUpstream, PathBuf, JoinHandle<Result<()>>) {
        let upstream = FakeUpstream::serving(vec![
            tool("github-get_issue", "Read one issue"),
            tool("github-create_issue", "Open an issue"),
        ]);
        let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
        let path = env::temp_dir()
            .join(format!("rm-{}", Uuid::new_v4().simple()))
            .join(SOCKET_FILE);
        let listener = bind(&path).expect("bind");
        let server = tokio::spawn(serve(service, listener, uid));
        (rig, upstream, path, server)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_give_a_refused_caller_nothing_and_never_reach_the_upstream() {
        let (rig, upstream, path, server) = start(daemon_uid());
        rig.callers.refuse(&["/usr/bin/curl"]);
        for request in [
            json!({"op": "search_tools", "query": "issue"}),
            json!({"op": "call_tool", "name": "github-get_issue", "confirmed": true}),
        ] {
            let reply = exchange(&path, request).await;
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["reason"], "caller_refused");
            assert!(reply.get("result").is_none());
        }
        assert_eq!(rig.callers.checks(), 2);
        assert!(upstream.lists().is_empty());
        assert!(upstream.calls().is_empty());
        server.abort();

        let (other_rig, other_upstream, other_path, other_server) = start(daemon_uid() + 1);
        let reply = exchange(&other_path, json!({"op": "search_tools", "query": "issue"})).await;
        assert_eq!(reply["reason"], "caller_refused");
        assert_eq!(other_rig.callers.checks(), 0);
        assert!(other_upstream.lists().is_empty());
        other_server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_ask_over_the_connection_and_act_on_the_answer_line() {
        let (_rig, upstream, path, server) = start(daemon_uid());
        let mut activation = Client::connect(&path).await;
        activation
            .send(json!({"op": "activate_server", "server": "github"}))
            .await;
        let question = activation.receive().await;
        assert_eq!(question["ask"]["kind"], "activate_server");
        assert!(question["ask"]["message"]
            .as_str()
            .expect("message")
            .contains("github"));
        activation.send(json!({"answer": "accept"})).await;
        assert_eq!(
            activation.receive().await,
            json!({"ok": true, "result": {"server": "github", "active": true}})
        );

        let call =
            json!({"op": "call_tool", "name": "github-create_issue", "arguments": {"title": "x"}});
        for (answer, reason) in [
            (json!({"answer": "decline"}), "declined"),
            (json!({"answer": "unsupported"}), "confirmation_required"),
            (json!({"answer": "maybe"}), "cancelled"),
        ] {
            let mut declined = Client::connect(&path).await;
            declined.send(call.clone()).await;
            assert_eq!(declined.receive().await["ask"]["kind"], "confirm_call");
            declined.send(answer).await;
            let reply = declined.receive().await;
            assert_eq!(reply["ok"], false);
            assert_eq!(reply["reason"], reason);
        }
        assert!(upstream.calls().is_empty());

        let mut accepted = Client::connect(&path).await;
        accepted.send(call).await;
        assert_eq!(accepted.receive().await["ask"]["kind"], "confirm_call");
        accepted.send(json!({"answer": "accept"})).await;
        let reply = accepted.receive().await;
        assert_eq!(reply["ok"], true);
        assert_eq!(
            reply["result"]["structuredContent"]["arguments"],
            json!({"title": "x"})
        );
        assert_eq!(upstream.calls().len(), 1);

        let malformed = exchange(
            &path,
            json!({"op": "call_tool", "name": "github-get_issue", "arguments": [1]}),
        )
        .await;
        assert_eq!(malformed["reason"], "bad_request");
        assert_eq!(upstream.calls().len(), 1);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_refuse_a_request_line_longer_than_four_mebibytes() {
        let (rig, upstream, path, server) = start(daemon_uid());
        let mut oversized = Client::connect(&path).await;
        let mut line = vec![b'a'; MAX_LINE_BYTES as usize + 1];
        line.push(b'\n');
        oversized
            .lines
            .writer
            .write_all(&line)
            .await
            .expect("write");
        let reply = oversized.receive().await;
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["reason"], "request_too_large");
        assert!(reply["message"]
            .as_str()
            .expect("message")
            .contains("4 MiB"));
        assert_eq!(rig.callers.checks(), 0);

        let activated = exchange(
            &path,
            json!({"op": "activate_server", "server": "github", "confirmed": true}),
        )
        .await;
        assert_eq!(activated["ok"], true);
        let body = "b".repeat(3 * 1024 * 1024);
        let large = exchange(
            &path,
            json!({"op": "call_tool", "name": "github-create_issue", "arguments": {"body": body}, "confirmed": true}),
        )
        .await;
        assert_eq!(large["ok"], true);
        let sent = upstream.calls().remove(0).2.expect("arguments");
        assert_eq!(sent["body"].as_str().map(str::len), Some(3 * 1024 * 1024));
        server.abort();
    }
}

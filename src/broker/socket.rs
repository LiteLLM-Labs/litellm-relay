//! The Unix socket the broker answers on: one JSON request line in, one JSON
//! reply line out, then close. The peer's uid must match the daemon's before
//! any request is read; `credential` then runs the caller check. The daemon
//! waits for the peer to hang up after the reply, so a refusal sent before the
//! request arrived still reaches a peer that is only now writing it.

use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    time::timeout,
};

use super::{
    caller::{Peer, AUDIT_TOKEN_BYTES},
    Broker, Refusal, Reply, Request,
};

const MAX_REQUEST_BYTES: u64 = 4096;
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub fn daemon_uid() -> u32 {
    unsafe { libc::getuid() }
}

pub struct PeerIdentity {
    pub uid: u32,
    pub pid: Option<u32>,
    pub audit_token: Option<[u8; AUDIT_TOKEN_BYTES]>,
}

pub fn bind(path: &Path) -> Result<UnixListener> {
    let dir = path
        .parent()
        .context("the broker socket path has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to restrict {}", dir.display()))?;
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to replace {}", path.display()))?;
    }
    let listener = UnixListener::bind(path)
        .with_context(|| format!("failed to listen on {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict {}", path.display()))?;
    Ok(listener)
}

pub async fn serve(broker: Arc<Broker>, listener: UnixListener, daemon_uid: u32) -> Result<()> {
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("the broker socket stopped accepting connections")?;
        let broker = Arc::clone(&broker);
        tokio::spawn(async move {
            let _ = handle(broker, stream, daemon_uid).await;
        });
    }
}

async fn handle(broker: Arc<Broker>, mut stream: UnixStream, daemon_uid: u32) -> Result<()> {
    let reply = match admit(identity(&stream), daemon_uid) {
        Err(refusal) => Reply::Refused(refusal),
        Ok(peer) => match read_request(&mut stream).await {
            Err(refusal) => Reply::Refused(refusal),
            Ok(request) => tokio::task::spawn_blocking(move || broker.handle(request, peer))
                .await
                .unwrap_or_else(|_| {
                    Reply::Refused(Refusal::GatewayError(
                        "the broker could not answer this request".to_string(),
                    ))
                }),
        },
    };
    let line = format!("{}\n", serde_json::to_string(&reply.to_json())?);
    stream.write_all(line.as_bytes()).await?;
    stream.shutdown().await?;
    let _ = timeout(REQUEST_READ_TIMEOUT, drain(&mut stream)).await;
    Ok(())
}

async fn drain(stream: &mut UnixStream) {
    let mut unread = (&mut *stream).take(MAX_REQUEST_BYTES);
    let _ = tokio::io::copy(&mut unread, &mut tokio::io::sink()).await;
}

async fn read_request(stream: &mut UnixStream) -> Result<Request, Refusal> {
    let mut line = String::new();
    let mut reader = BufReader::new((&mut *stream).take(MAX_REQUEST_BYTES));
    let read = timeout(REQUEST_READ_TIMEOUT, reader.read_line(&mut line)).await;
    match read {
        Ok(Ok(_)) => serde_json::from_str::<Request>(line.trim()).map_err(|_| {
            Refusal::BadRequest(
                "expected one JSON line such as {\"op\":\"credential\",\"context\":\"interactive\"}"
                    .to_string(),
            )
        }),
        Ok(Err(error)) => Err(Refusal::BadRequest(format!(
            "the request could not be read: {error}"
        ))),
        Err(_) => Err(Refusal::BadRequest(
            "no request line arrived in time".to_string(),
        )),
    }
}

pub(crate) fn identity(stream: &UnixStream) -> Option<PeerIdentity> {
    let credentials = stream.peer_cred().ok()?;
    Some(PeerIdentity {
        uid: credentials.uid(),
        pid: credentials.pid().and_then(|pid| u32::try_from(pid).ok()),
        audit_token: audit_token(stream),
    })
}

#[cfg(target_os = "macos")]
fn audit_token(stream: &UnixStream) -> Option<[u8; AUDIT_TOKEN_BYTES]> {
    use std::os::fd::AsRawFd;
    super::macos::peer_audit_token(stream.as_raw_fd())
}

#[cfg(not(target_os = "macos"))]
fn audit_token(_stream: &UnixStream) -> Option<[u8; AUDIT_TOKEN_BYTES]> {
    None
}

/// Only a process of the daemon's own uid gets as far as sending a request.
pub fn admit(identity: Option<PeerIdentity>, daemon_uid: u32) -> Result<Peer, Refusal> {
    let Some(identity) = identity else {
        return Err(Refusal::CallerRefused(
            "the peer's credentials could not be read".to_string(),
        ));
    };
    if identity.uid != daemon_uid {
        return Err(Refusal::CallerRefused(format!(
            "peer uid {} is not the daemon's uid {daemon_uid}",
            identity.uid
        )));
    }
    let Some(pid) = identity.pid else {
        return Err(Refusal::CallerRefused(
            "the peer's process id could not be read".to_string(),
        ));
    };
    Ok(Peer {
        pid,
        audit_token: identity.audit_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::SOCKET_FILE;
    use crate::broker::{
        test_support::{static_key_settings, Rig},
        Source,
    };
    use serde_json::Value;
    use std::{
        env,
        io::{BufRead, BufReader as StdBufReader, Write},
        os::unix::net::UnixStream as StdUnixStream,
        path::PathBuf,
    };
    use uuid::Uuid;

    fn scratch_socket() -> PathBuf {
        let dir = env::temp_dir().join(format!("rb-{}", Uuid::new_v4().simple()));
        dir.join(SOCKET_FILE)
    }

    fn exchange(path: &Path, line: &str) -> Value {
        let mut stream = StdUnixStream::connect(path).expect("connect");
        stream.write_all(line.as_bytes()).expect("write");
        let mut reply = String::new();
        StdBufReader::new(&stream)
            .read_line(&mut reply)
            .expect("read reply");
        serde_json::from_str(reply.trim()).expect("JSON reply")
    }

    async fn start(rig: &Rig, daemon_uid: u32) -> (PathBuf, tokio::task::JoinHandle<Result<()>>) {
        let path = scratch_socket();
        let listener = bind(&path).expect("bind");
        let broker = Arc::clone(&rig.broker);
        let server = tokio::spawn(serve(broker, listener, daemon_uid));
        (path, server)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_refuse_a_peer_of_another_uid_before_reading_any_request() {
        let rig = Rig::new(static_key_settings("sk-static"));
        let (path, server) = start(&rig, daemon_uid() + 1).await;
        let reply = tokio::task::spawn_blocking(move || {
            exchange(
                &path,
                "{\"op\":\"credential\",\"context\":\"interactive\"}\n",
            )
        })
        .await
        .expect("exchange");
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["reason"], "caller_refused");
        assert_eq!(rig.callers.checks(), 0);
        assert_eq!(rig.broker.status().refused_callers, 0);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_answer_a_malformed_line_with_a_refusal_and_no_token() {
        let rig = Rig::new(static_key_settings("sk-static"));
        let (path, server) = start(&rig, daemon_uid()).await;
        let reply = tokio::task::spawn_blocking(move || exchange(&path, "credential please\n"))
            .await
            .expect("exchange");
        assert_eq!(reply["ok"], false);
        assert_eq!(reply["reason"], "bad_request");
        assert!(reply.get("token").is_none());
        assert_eq!(rig.callers.checks(), 0);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_serve_the_credential_to_an_admitted_allowed_peer() {
        let rig = Rig::new(static_key_settings("sk-static"));
        let (path, server) = start(&rig, daemon_uid()).await;
        let reply = tokio::task::spawn_blocking(move || {
            exchange(
                &path,
                "{\"op\":\"credential\",\"context\":\"non_interactive\"}\n",
            )
        })
        .await
        .expect("exchange");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["token"], "sk-static");
        assert_eq!(reply["source"], Source::StaticKey.name());
        assert_eq!(rig.callers.checks(), 1);
        server.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_run_the_uid_only_ops_without_the_caller_check() {
        let rig = Rig::new(static_key_settings("sk-static"));
        let (path, server) = start(&rig, daemon_uid()).await;
        let status_path = path.clone();
        let reply =
            tokio::task::spawn_blocking(move || exchange(&status_path, "{\"op\":\"status\"}\n"))
                .await
                .expect("exchange");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["signed_in"], true);
        assert!(reply.get("token").is_none());
        let reply = tokio::task::spawn_blocking(move || exchange(&path, "{\"op\":\"sign_out\"}\n"))
            .await
            .expect("exchange");
        assert_eq!(reply["ok"], true);
        assert_eq!(rig.callers.checks(), 0);
        server.abort();
    }
}

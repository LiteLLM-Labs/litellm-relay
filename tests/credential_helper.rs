#![cfg(unix)]

use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Command, Output},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

use serde_json::{json, Value};

const RELAY: &str = env!("CARGO_BIN_EXE_litellm-relay");
static HOMES: AtomicUsize = AtomicUsize::new(0);

fn scratch_home() -> PathBuf {
    let home = env::temp_dir().join(format!(
        "rh-{}-{}",
        std::process::id(),
        HOMES.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).unwrap();
    home
}

struct FakeDaemon {
    home: PathBuf,
    request: Arc<Mutex<Option<String>>>,
    thread: JoinHandle<()>,
}

impl FakeDaemon {
    fn answering(reply: Value) -> Self {
        let home = scratch_home();
        let dir = home.join(".litellm-relay");
        fs::create_dir_all(&dir).unwrap();
        let listener = UnixListener::bind(dir.join("broker.sock")).unwrap();
        let request = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&request);
        let thread = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            *seen.lock().unwrap() = Some(line.trim().to_string());
            (&stream)
                .write_all(format!("{reply}\n").as_bytes())
                .unwrap();
        });
        Self {
            home,
            request,
            thread,
        }
    }

    fn request(self) -> Value {
        self.thread.join().unwrap();
        let line = self
            .request
            .lock()
            .unwrap()
            .clone()
            .expect("one request line");
        let _ = fs::remove_dir_all(&self.home);
        serde_json::from_str(&line).unwrap()
    }
}

fn relay(home: &PathBuf, args: &[&str], helper_context: Option<&str>) -> Output {
    let mut command = Command::new(RELAY);
    command
        .args(args)
        .env("HOME", home)
        .env_remove("CLAUDE_HELPER_CONTEXT");
    if let Some(context) = helper_context {
        command.env("CLAUDE_HELPER_CONTEXT", context);
    }
    command.output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn credential_prints_the_bare_token_the_daemon_answers_with() {
    let daemon =
        FakeDaemon::answering(json!({"ok": true, "token": "sk-test", "source": "minted_key"}));

    let output = relay(&daemon.home, &["credential"], None);

    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "sk-test\n");
    assert_eq!(
        daemon.request(),
        json!({"op": "credential", "context": "interactive"})
    );
}

#[test]
fn credential_sends_non_interactive_when_claude_desktop_runs_it_unattended() {
    let daemon =
        FakeDaemon::answering(json!({"ok": true, "token": "sk-test", "source": "minted_key"}));

    let output = relay(&daemon.home, &["credential"], Some("background"));

    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert_eq!(
        daemon.request(),
        json!({"op": "credential", "context": "non_interactive"})
    );
}

#[test]
fn credential_fails_with_one_stderr_line_and_no_stdout_when_refused() {
    let daemon = FakeDaemon::answering(json!({
        "ok": false,
        "reason": "caller_refused",
        "message": "caller refused: no allowed client in the process chain [zsh (1234)]"
    }));

    let output = relay(&daemon.home, &["credential"], None);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert_eq!(stderr.trim().lines().count(), 1, "stderr: {stderr}");
    assert!(stderr.contains("caller_refused"), "stderr: {stderr}");
    assert!(stderr.contains("zsh (1234)"), "stderr: {stderr}");
    daemon.request();
}

#[test]
fn credential_names_the_daemon_and_its_launch_agent_when_there_is_no_socket() {
    let home = scratch_home();

    let output = relay(&home, &["credential"], None);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(text(&output.stdout), "");
    let stderr = text(&output.stderr);
    assert!(stderr.contains("relay serve"), "stderr: {stderr}");
    assert!(stderr.contains("ai.litellm.relay"), "stderr: {stderr}");
    assert!(stderr.contains("broker.sock"), "stderr: {stderr}");
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn sign_out_asks_the_daemon_to_forget_the_session() {
    let daemon = FakeDaemon::answering(json!({"ok": true, "signed_in": false}));

    let output = relay(&daemon.home, &["sign-out"], None);

    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert!(text(&output.stdout).contains("Signed out"));
    assert_eq!(daemon.request(), json!({"op": "sign_out"}));
}

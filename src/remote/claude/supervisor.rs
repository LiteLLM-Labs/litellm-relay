use super::{
    super::protocol::{Decision, Failure, Outcome, Result},
    Claude,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs, io,
    os::unix::{
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::JoinSet,
};
use uuid::Uuid;

const LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello,
    List,
    Create {
        cwd: String,
    },
    Read {
        id: Uuid,
    },
    Send {
        id: Uuid,
        text: String,
        model: Option<String>,
    },
    Approve {
        id: Uuid,
        approval: Uuid,
        decision: Decision,
    },
    Interrupt {
        id: Uuid,
    },
    HandoffChoices,
    Handoff {
        id: Uuid,
    },
}

impl Request {
    pub async fn execute(self, claude: &Claude) -> Result<Value> {
        fn encode(value: impl Serialize) -> Result<Value> {
            serde_json::to_value(value)
                .map_err(|_| Failure::new(500, "Cannot encode Claude response."))
        }
        match self {
            Self::Hello => {
                Ok(json!({"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION")}))
            }
            Self::List => encode(claude.list().await?),
            Self::Create { cwd } => encode(claude.create(cwd)?),
            Self::Read { id } => encode(claude.read(id).await?.bounded()),
            Self::Send { id, text, model } => claude.send(id, text, model).await,
            Self::Approve {
                id,
                approval,
                decision,
            } => claude.approve(id, approval, decision).await,
            Self::Interrupt { id } => claude.interrupt(id).await,
            Self::HandoffChoices => encode(claude.handoff_choices().await?),
            Self::Handoff { id } => {
                if claude.has_open_run(id) {
                    return Err(Failure::new(
                        409,
                        "This session is still open in the Claude supervisor.",
                    ));
                }
                claude.handoff(id).await?;
                Ok(Value::Null)
            }
        }
    }
}

pub fn default_socket(device: Uuid) -> PathBuf {
    crate::config::relay_home().join(format!("remote/claude-{device}.d/supervisor.sock"))
}

fn private_directory(socket: &Path) -> anyhow::Result<&Path> {
    anyhow::ensure!(
        socket.as_os_str().as_encoded_bytes().len() < 108,
        "Claude supervisor socket path exceeds the Unix 107-byte limit: {}",
        socket.display()
    );
    let parent = socket
        .parent()
        .context("Claude socket needs a parent directory.")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    Ok(parent)
}

pub struct Server {
    listener: UnixListener,
    _lock: fs::File,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Forked children can hold this file description until exec.
        if let Err(error) = self._lock.unlock() {
            eprintln!("Cannot unlock Claude supervisor: {error}");
        }
    }
}

impl Server {
    pub fn bind(socket: &Path) -> anyhow::Result<Option<Self>> {
        let parent = private_directory(socket)?;
        let path = parent.join("supervisor.lock");
        match crate::system::create_private(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let lock = fs::File::open(path)?;
        if let Err(error) = lock.try_lock() {
            if matches!(error, std::fs::TryLockError::WouldBlock) {
                eprintln!("Claude supervisor is already running.");
                return Ok(None);
            }
            return Err(error.into());
        }
        match fs::remove_file(socket) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(socket)?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
        Ok(Some(Self {
            listener,
            _lock: lock,
        }))
    }

    pub async fn serve(
        self,
        claude: Arc<Claude>,
        idle_exit: bool,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> anyhow::Result<()> {
        tokio::pin!(shutdown);
        let mut tasks = JoinSet::new();
        let mut last = tokio::time::Instant::now();
        let mut tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = tick.tick() => {
                    if idle_exit && tasks.is_empty() && last.elapsed() >= Duration::from_secs(900) && !claude.has_open_runs() { break; }
                }
                Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = self.listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(connection) => connection,
                        Err(error) => {
                            eprintln!("Cannot accept Claude supervisor connection: {error}");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    };
                    let credentials = match stream.peer_cred() {
                        Ok(credentials) => credentials,
                        Err(error) if error.kind() == io::ErrorKind::NotConnected => continue,
                        Err(error) => {
                            eprintln!("Cannot identify Claude supervisor peer: {error}");
                            continue;
                        }
                    };
                    if credentials.uid() != unsafe { libc::geteuid() } { continue; }
                    last = tokio::time::Instant::now();
                    let claude = claude.clone();
                    tasks.spawn(async move {
                        let mut stream = BufReader::new(stream);
                        let response = match read_line(&mut stream).await.and_then(|line| serde_json::from_slice::<Request>(&line).map_err(|_| Failure::new(400, "Invalid Claude request."))) {
                            Ok(request) => request.execute(&claude).await,
                            Err(error) => Err(error),
                        };
                        if let Ok(mut bytes) = serde_json::to_vec(&Outcome::from_result(response)) {
                            bytes.push(b'\n');
                            if bytes.len() as u64 > LIMIT {
                                bytes = serde_json::to_vec(&Outcome::from_result(Err(Failure::new(502, "Claude response exceeds 8 MB.")))).expect("failure serialization");
                                bytes.push(b'\n');
                            }
                            let _ = tokio::time::timeout(Duration::from_secs(120), stream.get_mut().write_all(&bytes)).await;
                        }
                    });
                }
            }
        }
        while tasks.join_next().await.is_some() {}
        claude.close_runs().await;
        Ok(())
    }
}

async fn read_line(stream: &mut BufReader<UnixStream>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(120),
        stream.take(LIMIT + 1).read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(|_| Failure::new(504, "Claude supervisor timed out."))?
    .map_err(|_| Failure::new(503, "Claude supervisor disconnected."))?;
    if bytes.len() as u64 > LIMIT || bytes.last() != Some(&b'\n') {
        return Err(Failure::new(
            400,
            "Claude protocol line is missing or exceeds 8 MB.",
        ));
    }
    Ok(bytes)
}

pub struct Client {
    socket: PathBuf,
    auto_start: bool,
    executable: Option<PathBuf>,
}

impl Client {
    pub fn new(socket: PathBuf, auto_start: bool, executable: Option<PathBuf>) -> Self {
        Self {
            socket,
            auto_start,
            executable,
        }
    }

    fn spawn(&self) -> anyhow::Result<()> {
        let parent = private_directory(&self.socket)?;
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(parent.join("supervisor.log"))?;
        fs::set_permissions(
            parent.join("supervisor.log"),
            fs::Permissions::from_mode(0o600),
        )?;
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command
            .args(["remote", "claude-supervisor", "--idle-exit"])
            .arg("--socket")
            .arg(&self.socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        if let Some(executable) = &self.executable {
            command.arg("--claude-executable").arg(executable);
        }
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    pub async fn call(&self, request: Request) -> Result<Value> {
        let connected =
            tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(&self.socket)).await;
        let mut stream = match connected {
            Ok(Ok(stream)) => stream,
            Ok(Err(error))
                if self.auto_start
                    && cfg!(target_os = "linux")
                    && matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
            {
                self.spawn().map_err(|error| {
                    Failure::new(503, format!("Cannot start Claude supervisor: {error}"))
                })?;
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        if let Ok(stream) = UnixStream::connect(&self.socket).await {
                            break stream;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .map_err(|_| {
                    Failure::new(
                        503,
                        "Claude supervisor did not start. Check supervisor.log next to its socket.",
                    )
                })?
            }
            _ => return Err(Failure::new(503, "Cannot connect to Claude supervisor.")),
        };
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|_| Failure::new(400, "Invalid Claude request."))?;
        bytes.push(b'\n');
        if bytes.len() as u64 > LIMIT {
            return Err(Failure::new(400, "Claude request exceeds 8 MB."));
        }
        tokio::time::timeout(Duration::from_secs(120), stream.write_all(&bytes))
            .await
            .map_err(|_| Failure::new(504, "Claude supervisor timed out."))?
            .map_err(|_| Failure::new(503, "Claude supervisor disconnected."))?;
        let bytes = read_line(&mut BufReader::new(stream)).await?;
        serde_json::from_slice::<Outcome>(&bytes)
            .map_err(|_| Failure::new(502, "Invalid Claude supervisor response."))?
            .into_result()
    }
}

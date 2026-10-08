use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use rmcp::{
    model::{CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation},
    transport::{ConfigureCommandExt, TokioChildProcess},
    ServiceExt,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader},
    net::UnixListener,
};

const RELAY: &str = env!("CARGO_BIN_EXE_litellm-relay");

fn scratch_home(label: &str) -> PathBuf {
    let home = env::temp_dir().join(format!("rmh-{}-{label}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(home.join(".litellm-relay")).unwrap();
    home
}

fn stand_in_daemon(home: &Path) -> Arc<Mutex<Vec<Value>>> {
    let listener = UnixListener::bind(home.join(".litellm-relay").join("mcp.sock")).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let (reader, mut writer) = stream.into_split();
                let mut lines = AsyncBufReader::new(reader).lines();
                let request: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                seen.lock().unwrap().push(request);
                let reply = json!({"ok": true, "result": {"tools": ["github-list_issues"], "inactive_servers": []}});
                writer
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
                writer.shutdown().await.unwrap();
            });
        }
    });
    requests
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_mcp_serves_the_six_tools_over_stdio_and_relays_a_call_to_the_daemon_socket() {
    let home = scratch_home("client");
    let requests = stand_in_daemon(&home);
    let transport = TokioChildProcess::new(tokio::process::Command::new(RELAY).configure(|cmd| {
        cmd.arg("mcp").env("HOME", &home).stderr(Stdio::inherit());
    }))
    .unwrap();
    let client_info = ClientConfig::new(
        ClientCapabilities::builder().enable_elicitation().build(),
        Implementation::new("stdio-test", "0"),
    );
    let client = client_info.serve(transport).await.unwrap();

    let server = client.peer_info().unwrap();
    let implementation = server.server_info.as_ref().unwrap();
    assert_eq!(implementation.name, "litellm-relay");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
    assert!(server.capabilities.tools.is_some());

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "search_tools",
            "describe_tool",
            "call_tool",
            "activate_server",
            "switch_team",
            "switch_environment"
        ]
    );
    let read_only: Vec<Option<bool>> = tools
        .iter()
        .map(|tool| tool.annotations.as_ref().unwrap().read_only_hint)
        .collect();
    assert_eq!(
        read_only,
        [
            Some(true),
            Some(true),
            Some(false),
            Some(false),
            Some(false),
            Some(false)
        ]
    );
    assert_eq!(
        tools[2].annotations.as_ref().unwrap().open_world_hint,
        Some(true)
    );

    let result = client
        .call_tool(
            CallToolRequestParams::new("search_tools")
                .with_arguments(json!({"query": "issues"}).as_object().cloned().unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.structured_content,
        Some(json!({"tools": ["github-list_issues"], "inactive_servers": []}))
    );
    assert_eq!(
        *requests.lock().unwrap(),
        vec![json!({"op": "search_tools", "query": "issues"})]
    );

    client.cancel().await.unwrap();
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn relay_mcp_answers_a_pre_initialize_server_discover_and_keeps_the_connection() {
    let home = scratch_home("discover");
    let mut child = Command::new(RELAY)
        .arg("mcp")
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut reply = String::new();

    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"server/discover\",\"params\":{}}\n")
        .unwrap();
    stdout.read_line(&mut reply).unwrap();
    let discover: Value = serde_json::from_str(reply.trim()).unwrap();
    assert_eq!(discover["id"], 0);
    assert!(
        discover.get("result").is_some() || discover["error"]["code"].is_i64(),
        "server/discover must be answered with a result or a JSON-RPC error, never dropped: {discover}"
    );

    reply.clear();
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"raw\",\"version\":\"0\"}}}\n")
        .unwrap();
    stdout.read_line(&mut reply).unwrap();
    let initialized: Value = serde_json::from_str(reply.trim()).unwrap();
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "litellm-relay");

    drop(stdin);
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "relay mcp exits cleanly when stdin closes: {status}"
    );
    let _ = fs::remove_dir_all(&home);
}

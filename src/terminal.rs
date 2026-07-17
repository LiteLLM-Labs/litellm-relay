use std::{env, path::Path};

use serde_json::Value;

use crate::config::RelayConfig;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";

pub fn print_banner() {
    let title = [
        "  ___ _ _       _    _     __  __        ___      _             ",
        " | _ (_) |_ ___| |  | |   |  \\/  |___   | _ \\___ | |__ _ _  _  ",
        " |  _/ |  _/ -_) |__| |__ | |\\/| / -_)  |   / -_)| / _` | || | ",
        " |_| |_|\\__\\___|____|____||_|  |_\\___|  |_|_\\___||_\\__,_|\\_, | ",
        "                                                           |__/  ",
    ];

    println!();
    for line in title {
        println!("{}{}{}", color(CYAN), line, color(RESET));
    }
    println!(
        "{}                 litellm-relay{}",
        color(BOLD),
        color(RESET)
    );
    println!(
        "{}local traffic relay for LiteLLM Gateway{}",
        color(DIM),
        color(RESET)
    );
    println!();
}

pub fn print_setup_intro() {
    print_banner();
    println!("{}Setup wizard{}", color(BOLD), color(RESET));
    println!("This will connect Relay to your LiteLLM Gateway and prepare local tracing.");
    println!();
}

pub fn print_step(number: u8, total: u8, title: &str) {
    println!(
        "{}Step {number} of {total}{}  {title}",
        color(CYAN),
        color(RESET)
    );
}

pub fn print_setup_complete(config_path: &Path, user_id: Option<&str>, team_id: Option<&str>) {
    println!("  Config: {}", config_path.display());
    if let Some(user_id) = user_id {
        println!("  User: {user_id}");
    }
    if let Some(team_id) = team_id {
        println!("  Team: {team_id}");
    }
    println!();
    println!(
        "{}Setup complete.{} Relay is ready to start.",
        color(GREEN),
        color(RESET)
    );
}

pub fn print_usage_access_warning(denied_routes: &[&str], suggestion: &str) {
    println!();
    println!(
        "{}Heads up:{} this key can't read usage yet, so the RelayBar menu bar app will show empty tabs.",
        color(YELLOW),
        color(RESET)
    );
    println!("  Denied: {}", denied_routes.join(", "));
    println!("  {suggestion}");
}

pub fn print_runtime_panel(config: &RelayConfig) {
    print_banner();
    println!("{}Relay is running{}", color(BOLD), color(RESET));
    println!("  UI dashboard:  http://{}:{}/", config.host, config.port);
    println!(
        "  PAC file:      http://{}:{}/proxy.pac",
        config.host, config.port
    );
    println!("  Proxy:         {}:{}", config.host, config.port);
    println!(
        "  Gateway:       {}",
        config.gateway_url.trim_end_matches('/')
    );
    println!("  Log file:      {}", config.log_path.display());
    println!(
        "  Payload trace: {}",
        enabled_label(config.mitm_enabled, "active", "metadata only")
    );
    println!(
        "  Gateway sync:  {}",
        enabled_label(
            config.gateway_api_key.is_some(),
            "authenticated",
            "not authenticated"
        )
    );
    println!(
        "  Shadow calls:  {}",
        enabled_label(config.shadow_enabled, "enabled", "disabled")
    );
    println!();
    println!(
        "{}Actively tracing configured AI traffic. Recent events will appear below.{}",
        color(GREEN),
        color(RESET)
    );
    println!(
        "{}To route apps through Relay, use the PAC URL above or run the installer with --set-system-proxy.{}",
        color(DIM),
        color(RESET)
    );
    println!();
}

pub fn print_trace_event(event: &Value) {
    let event_name = event.get("event").and_then(Value::as_str).unwrap_or("");
    match event_name {
        "connect" => print_connect(event),
        "connect_closed" => print_connect_closed(event),
        "http_request" => print_http_request(event),
        "http_response" => print_http_response(event),
        "gateway_ingest" => print_gateway_ingest(event),
        "payload_capture_failed" | "connect_failed" | "client_error" => print_error(event),
        _ => {}
    }
}

fn print_connect(event: &Value) {
    let host = str_field(event, "host");
    let app = str_field(event, "app");
    let port = event.get("port").and_then(Value::as_u64).unwrap_or(443);
    let ai = event
        .get("ai_match")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let marker = if ai { color(GREEN) } else { color(DIM) };
    println!(
        "{}trace{}   CONNECT {:<10} {}:{}",
        marker,
        color(RESET),
        app,
        host,
        port
    );
}

fn print_connect_closed(event: &Value) {
    let host = str_field(event, "host");
    let duration = event
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let bytes_in = event
        .get("bytes_in")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let bytes_out = event
        .get("bytes_out")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    println!(
        "{}done{}    {} duration={}ms in={}B out={}B",
        color(DIM),
        color(RESET),
        host,
        duration,
        bytes_in,
        bytes_out
    );
}

fn print_http_request(event: &Value) {
    let method = str_field(event, "method");
    let host = str_field(event, "host");
    let path = str_field(event, "path");
    println!(
        "{}capture{} request  {:<6} {}{}",
        color(CYAN),
        color(RESET),
        method,
        host,
        path
    );
}

fn print_http_response(event: &Value) {
    let status = event
        .get("status_code")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let host = str_field(event, "host");
    let path = str_field(event, "path");
    let status_color = if status >= 400 {
        color(RED)
    } else {
        color(GREEN)
    };
    println!(
        "{}capture{} response {}{}{} {}{}",
        color(CYAN),
        color(RESET),
        status_color,
        status,
        color(RESET),
        host,
        path
    );
}

fn print_gateway_ingest(event: &Value) {
    let ok = event.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let host = str_field(event, "host");
    let path = str_field(event, "path");
    if ok {
        println!(
            "{}gateway{} synced   {}{}",
            color(GREEN),
            color(RESET),
            host,
            path
        );
    } else {
        let status = event
            .get("status")
            .and_then(Value::as_u64)
            .map(|status| status.to_string())
            .unwrap_or_else(|| "error".into());
        println!(
            "{}gateway{} pending  {}{} ({})",
            color(YELLOW),
            color(RESET),
            host,
            path,
            status
        );
    }
}

fn print_error(event: &Value) {
    let event_name = str_field(event, "event");
    let host = str_field(event, "host");
    let error = str_field(event, "error");
    println!(
        "{}error{}   {} {} {}",
        color(RED),
        color(RESET),
        event_name,
        host,
        error
    );
}

fn enabled_label(enabled: bool, on: &str, off: &str) -> String {
    if enabled {
        format!("{}{}{}", color(GREEN), on, color(RESET))
    } else {
        format!("{}{}{}", color(YELLOW), off, color(RESET))
    }
}

fn str_field<'a>(event: &'a Value, field: &str) -> &'a str {
    event.get(field).and_then(Value::as_str).unwrap_or("-")
}

fn color(code: &'static str) -> &'static str {
    if env::var_os("NO_COLOR").is_some() {
        ""
    } else {
        code
    }
}

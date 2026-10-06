use std::{borrow::Cow, collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, Tool},
    service::{RoleClient, RunningService, ServiceError},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    ServiceExt,
};
use serde_json::{Map, Value};
use tokio::{sync::Mutex, time::timeout};

pub const CREDENTIAL_HEADER: &str = "x-litellm-api-key";
pub const GATEWAY_MCP_PATH: &str = "/mcp";
pub const CONNECT_CEILING: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayTarget {
    pub url: String,
    pub credential: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpstreamTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub annotations: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamError(pub String);

pub type UpstreamFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, UpstreamError>> + Send + 'a>>;

pub trait Upstream: Send + Sync {
    fn list_tools<'a>(&'a self, target: &'a GatewayTarget)
        -> UpstreamFuture<'a, Vec<UpstreamTool>>;

    fn call_tool<'a>(
        &'a self,
        target: &'a GatewayTarget,
        name: &'a str,
        arguments: Option<Map<String, Value>>,
    ) -> UpstreamFuture<'a, Value>;
}

type Connection = RunningService<RoleClient, ()>;

pub struct RmcpUpstream {
    connected: Mutex<Option<(GatewayTarget, Arc<Connection>)>>,
    connect_ceiling: Duration,
}

impl Default for RmcpUpstream {
    fn default() -> Self {
        Self::with_connect_ceiling(CONNECT_CEILING)
    }
}

impl RmcpUpstream {
    pub fn with_connect_ceiling(connect_ceiling: Duration) -> Self {
        Self {
            connected: Mutex::new(None),
            connect_ceiling,
        }
    }

    async fn connection(&self, target: &GatewayTarget) -> Result<Arc<Connection>, UpstreamError> {
        let mut connected = self.connected.lock().await;
        let reusable = connected
            .as_ref()
            .filter(|(cached_for, connection)| cached_for == target && !connection.is_closed())
            .map(|(_, connection)| Arc::clone(connection));
        if let Some(connection) = reusable {
            return Ok(connection);
        }
        let connection = match timeout(self.connect_ceiling, connect(target)).await {
            Ok(connected) => Arc::new(connected?),
            Err(_) => {
                return Err(UpstreamError(format!(
                    "the Gateway MCP server did not finish initialize within {} ms",
                    self.connect_ceiling.as_millis()
                )))
            }
        };
        *connected = Some((target.clone(), Arc::clone(&connection)));
        Ok(connection)
    }

    async fn forget_after(&self, connection: &Arc<Connection>, error: &ServiceError) {
        if matches!(error, ServiceError::McpError(_)) {
            return;
        }
        let mut connected = self.connected.lock().await;
        let is_the_cached_one = connected
            .as_ref()
            .is_some_and(|(_, cached)| Arc::ptr_eq(cached, connection));
        if is_the_cached_one {
            *connected = None;
        }
    }
}

impl Upstream for RmcpUpstream {
    fn list_tools<'a>(
        &'a self,
        target: &'a GatewayTarget,
    ) -> UpstreamFuture<'a, Vec<UpstreamTool>> {
        Box::pin(async move {
            let connection = self.connection(target).await?;
            let tools = match connection.list_all_tools().await {
                Ok(tools) => tools,
                Err(error) => {
                    self.forget_after(&connection, &error).await;
                    return Err(UpstreamError(format!(
                        "the Gateway did not list its MCP tools: {error}"
                    )));
                }
            };
            tools.into_iter().map(upstream_tool).collect()
        })
    }

    fn call_tool<'a>(
        &'a self,
        target: &'a GatewayTarget,
        name: &'a str,
        arguments: Option<Map<String, Value>>,
    ) -> UpstreamFuture<'a, Value> {
        Box::pin(async move {
            let connection = self.connection(target).await?;
            let request = CallToolRequestParams::new(name.to_string());
            let request = match arguments {
                Some(arguments) => request.with_arguments(arguments),
                None => request,
            };
            match connection.call_tool_once(request).await {
                Ok(CallToolResponse::Complete(result)) => {
                    serde_json::to_value(result).map_err(|error| {
                        UpstreamError(format!("the tool result could not be encoded: {error}"))
                    })
                }
                Ok(_) => Err(UpstreamError(format!(
                    "the Gateway answered {name} with a request for more input or a task, which Relay does not relay"
                ))),
                Err(error) => {
                    self.forget_after(&connection, &error).await;
                    Err(UpstreamError(format!("the Gateway did not run {name}: {error}")))
                }
            }
        })
    }
}

async fn connect(target: &GatewayTarget) -> Result<Connection, UpstreamError> {
    let credential =
        HeaderValue::from_str(&format!("Bearer {}", target.credential)).map_err(|_| {
            UpstreamError("the Gateway credential cannot be sent in a header".to_string())
        })?;
    let endpoint = format!("{}{GATEWAY_MCP_PATH}", target.url.trim_end_matches('/'));
    let config = StreamableHttpClientTransportConfig::with_uri(endpoint.clone())
        .custom_headers(HashMap::from([(
            HeaderName::from_static(CREDENTIAL_HEADER),
            credential,
        )]))
        .max_concurrent_requests(usize::MAX);
    ().serve(StreamableHttpClientTransport::from_config(config))
        .await
        .map_err(|error| UpstreamError(format!("could not connect to {endpoint}: {error}")))
}

fn upstream_tool(tool: Tool) -> Result<UpstreamTool, UpstreamError> {
    let annotations = tool
        .annotations
        .map(serde_json::to_value)
        .transpose()
        .map_err(|error| {
            UpstreamError(format!(
                "the annotations of {} could not be read: {error}",
                tool.name
            ))
        })?;
    Ok(UpstreamTool {
        name: tool.name.into_owned(),
        description: tool.description.map(Cow::into_owned),
        input_schema: Value::Object((*tool.input_schema).clone()),
        annotations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    use axum::{extract::Request, http::StatusCode, middleware, response::IntoResponse, Router};
    use rmcp::{
        model::{
            CallToolResult, ErrorData, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
            ServerConfig, ToolAnnotations,
        },
        service::{RequestContext, RoleServer},
        transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        },
        ServerHandler,
    };
    use serde_json::json;

    const CREDENTIAL: &str = "sk-relay-test";

    #[derive(Clone, Default)]
    struct StandIn {
        calls: Arc<StdMutex<Vec<CallToolRequestParams>>>,
    }

    fn schema() -> Map<String, Value> {
        json!({"type": "object", "properties": {"id": {"type": "integer"}}})
            .as_object()
            .cloned()
            .expect("object")
    }

    impl ServerHandler for StandIn {
        fn get_info(&self) -> ServerConfig {
            ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn list_tools(
            &self,
            request: Option<PaginatedRequestParams>,
            _context: RequestContext<RoleServer>,
        ) -> Result<ListToolsResult, ErrorData> {
            let cursor = request.and_then(|request| request.cursor);
            let (tools, next_cursor) = match cursor.as_deref() {
                None => (
                    vec![Tool::new("github-get_issue", "Read one issue", schema())
                        .with_annotations(ToolAnnotations::new().read_only(true))],
                    Some("page-2".to_string()),
                ),
                Some("page-2") => (
                    vec![Tool::new("github-create_issue", "Open an issue", schema())],
                    Some("page-3".to_string()),
                ),
                Some(_) => (
                    vec![Tool::new("jira-search", "Search issues", schema())],
                    None,
                ),
            };
            Ok(ListToolsResult {
                tools,
                next_cursor,
                ..ListToolsResult::default()
            })
        }

        async fn call_tool(
            &self,
            request: CallToolRequestParams,
            _context: RequestContext<RoleServer>,
        ) -> Result<CallToolResponse, ErrorData> {
            self.calls.lock().expect("calls").push(request.clone());
            Ok(CallToolResult::structured(json!({
                "tool": request.name,
                "received": request.arguments,
                "nested": {"list": [1, 2, {"deep": true}]},
            }))
            .into())
        }
    }

    async fn start(stand_in: StandIn, headers: Arc<StdMutex<Vec<Option<String>>>>) -> String {
        let service: StreamableHttpService<StandIn, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(stand_in.clone()),
                Default::default(),
                StreamableHttpServerConfig::default(),
            );
        let router = Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn(
                move |request: Request, next: middleware::Next| {
                    let headers = Arc::clone(&headers);
                    async move {
                        let presented = request
                            .headers()
                            .get("x-litellm-api-key")
                            .and_then(|value| value.to_str().ok())
                            .map(str::to_string);
                        let accepted = presented.as_deref() == Some("Bearer sk-relay-test");
                        headers.lock().expect("headers").push(presented);
                        match accepted {
                            true => next.run(request).await,
                            false => StatusCode::UNAUTHORIZED.into_response(),
                        }
                    }
                },
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        format!("http://{address}/")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_list_every_page_and_call_a_tool_on_a_real_streamable_http_server() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let stand_in = StandIn::default();
        let headers = Arc::new(StdMutex::new(Vec::new()));
        let url = start(stand_in.clone(), Arc::clone(&headers)).await;
        let target = GatewayTarget {
            url,
            credential: CREDENTIAL.to_string(),
        };
        let upstream = RmcpUpstream::default();

        let tools = upstream.list_tools(&target).await.expect("tools");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["github-get_issue", "github-create_issue", "jira-search"]
        );
        assert_eq!(tools[0].description.as_deref(), Some("Read one issue"));
        assert_eq!(tools[0].input_schema, Value::Object(schema()));
        assert_eq!(tools[0].annotations, Some(json!({"readOnlyHint": true})));
        assert_eq!(tools[1].annotations, None);

        let arguments =
            json!({"id": 7, "labels": ["a", "b"], "body": {"text": "h\u{e9}llo", "draft": null}});
        let result = upstream
            .call_tool(
                &target,
                "github-create_issue",
                arguments.as_object().cloned(),
            )
            .await
            .expect("result");
        let expected_structure = json!({
            "tool": "github-create_issue",
            "received": arguments,
            "nested": {"list": [1, 2, {"deep": true}]},
        });
        assert_eq!(result["structuredContent"], expected_structure);
        assert_eq!(result["isError"], json!(false));
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(
            serde_json::from_str::<Value>(result["content"][0]["text"].as_str().expect("text"))
                .expect("JSON text"),
            expected_structure
        );
        let calls = stand_in.calls.lock().expect("calls").clone();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "github-create_issue");
        assert_eq!(calls[0].arguments, arguments.as_object().cloned());

        let presented = headers.lock().expect("headers").clone();
        assert!(presented.len() >= 5, "{presented:?}");
        assert!(presented
            .iter()
            .all(|header| header.as_deref() == Some("Bearer sk-relay-test")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_give_up_on_a_gateway_that_accepts_tcp_but_never_answers_initialize() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let silent = tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                held.push(socket);
            }
        });
        let target = GatewayTarget {
            url,
            credential: "sk-relay-test".to_string(),
        };
        let upstream = RmcpUpstream::with_connect_ceiling(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let outcome = timeout(Duration::from_secs(10), upstream.list_tools(&target)).await;
        silent.abort();
        let error = outcome
            .expect("the connect ceiling must end the call")
            .expect_err("a silent Gateway cannot list tools");
        assert!(error.0.contains("initialize"), "{}", error.0);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_answer_an_error_when_the_gateway_refuses_the_credential() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let stand_in = StandIn::default();
        let headers = Arc::new(StdMutex::new(Vec::new()));
        let url = start(stand_in.clone(), Arc::clone(&headers)).await;
        let target = GatewayTarget {
            url,
            credential: "sk-someone-else".to_string(),
        };
        let upstream = RmcpUpstream::default();
        assert!(upstream.list_tools(&target).await.is_err());
        assert!(upstream
            .call_tool(&target, "github-get_issue", None)
            .await
            .is_err());
        assert!(stand_in.calls.lock().expect("calls").is_empty());
    }
}

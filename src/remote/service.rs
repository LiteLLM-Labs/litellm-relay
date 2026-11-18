use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{
        connect_info::ConnectInfo, ws::WebSocketUpgrade, DefaultBodyLimit, Json, Path as RoutePath,
        State,
    },
    http::{HeaderMap, Request},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use url::Url;
use uuid::Uuid;

use super::{
    auth::Authentication,
    peers::Peers,
    protocol::{digest, now_ms, split_session, Action, Agent, Decision, Failure, Result},
    store::{Identity, RequestState, Store},
};

pub struct Service {
    store: Arc<Store>,
    auth: Authentication,
    peers: Arc<Peers>,
    gateway: Url,
    public: Url,
    attempts: Mutex<HashMap<IpAddr, (u32, i64)>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PollInput {
    flow_token: String,
    team: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollInput {
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CreateInput {
    request_id: Uuid,
    cwd: String,
    agent: Agent,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MessageInput {
    request_id: Uuid,
    text: String,
    model: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalInput {
    decision: Decision,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InterruptInput {
    request_id: Uuid,
}
#[derive(Deserialize)]
struct ListInput {
    cursor: Option<String>,
}

impl Service {
    pub fn open(gateway: Url, public: Url, path: &Path) -> Result<Arc<Self>> {
        let store = Arc::new(Store::open(path)?);
        Ok(Arc::new(Self {
            auth: Authentication::new(gateway.clone(), store.clone()),
            peers: Arc::new(Peers::new(store.clone())),
            store,
            gateway,
            public,
            attempts: Mutex::new(HashMap::new()),
        }))
    }

    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
            .route("/.well-known/litellm-remote.json", get(discovery))
            .route("/api/me", get(identity))
            .route("/api/auth/start", post(start))
            .route("/api/auth/poll", post(poll))
            .route("/api/auth/logout", post(logout))
            .route("/api/devices", get(devices).post(enroll))
            .route("/api/devices/{id}", axum::routing::delete(remove))
            .route("/api/devices/{id}/models", get(models))
            .route("/api/devices/{id}/sessions", get(sessions).post(create))
            .route("/api/devices/{device}/sessions/{id}", get(detail))
            .route(
                "/api/devices/{device}/sessions/{id}/messages",
                post(message),
            )
            .route(
                "/api/devices/{device}/sessions/{id}/interrupt",
                post(interrupt),
            )
            .route(
                "/api/devices/{device}/sessions/{id}/approvals/{approval}",
                post(approve),
            )
            .route("/agent", get(agent))
            .fallback(|| async { Failure::new(404, "Not found.") })
            .layer(DefaultBodyLimit::max(131_072))
            .layer(middleware::from_fn_with_state(self.clone(), guard))
            .layer(middleware::map_response(
                |mut response: Response| async move {
                    response
                        .headers_mut()
                        .insert("Cache-Control", "no-store".parse().expect("header"));
                    response
                        .headers_mut()
                        .insert("X-Content-Type-Options", "nosniff".parse().expect("header"));
                    response
                        .headers_mut()
                        .insert("Referrer-Policy", "no-referrer".parse().expect("header"));
                    response
                },
            ))
            .with_state(self)
    }

    fn identity(&self, headers: &HeaderMap) -> Result<(Identity, String)> {
        let token = bearer(headers)
            .ok_or_else(|| Failure::new(401, "Sign in to your LiteLLM workspace."))?;
        self.store
            .identity(&token, now_ms())?
            .map(|identity| (identity, token))
            .ok_or_else(|| Failure::new(401, "Sign in to your LiteLLM workspace."))
    }

    fn rate_limit(&self, address: IpAddr) -> Result<()> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| Failure::new(503, "Sign-in is unavailable."))?;
        attempts.retain(|_, (_, until)| *until > now_ms());
        if attempts.len() >= 1000 {
            return Err(Failure::new(
                429,
                "Too many sign-in attempts. Wait a minute.",
            ));
        }
        let (count, _) = attempts.entry(address).or_insert((0, now_ms() + 60_000));
        *count += 1;
        if *count > 100 {
            return Err(Failure::new(
                429,
                "Too many sign-in attempts. Wait a minute.",
            ));
        }
        Ok(())
    }

    async fn request(&self, headers: &HeaderMap, device: Uuid, action: Action) -> Result<Value> {
        let (identity, _) = self.identity(headers)?;
        let device = self.store.device(device, &identity.subject)?;
        self.peers.request(&device, action).await
    }

    async fn once(
        &self,
        headers: &HeaderMap,
        device: Uuid,
        request: Uuid,
        action: Action,
    ) -> Result<Value> {
        let (identity, _) = self.identity(headers)?;
        let device = self.store.device(device, &identity.subject)?;
        let fingerprint = digest(
            &serde_json::to_string(&(device.id.to_string(), &action))
                .map_err(|_| Failure::new(400, "Invalid request."))?,
        );
        match self
            .store
            .begin(&identity.subject, request, &fingerprint, now_ms())?
        {
            RequestState::Cached(outcome) => return outcome.into_result(),
            RequestState::Unconfirmed => {
                return Err(Failure::new(
                    409,
                    "This request was already submitted. Refresh the session before sending again.",
                ))
            }
            RequestState::New => {}
        }
        let pending = match self.peers.dispatch(&device, action) {
            Ok(pending) => pending,
            Err(error) => {
                self.store.cancel_pending(&identity.subject, request)?;
                return Err(error);
            }
        };
        let result = self.peers.receive(&device, pending).await;
        self.store
            .finish(&identity.subject, request, result.clone())?;
        result
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get("Authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    (token.len() == 43
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
    .then(|| token.to_owned())
}

async fn guard(
    State(service): State<Arc<Service>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if path == "/healthz" || path == "/.well-known/litellm-remote.json" {
        return next.run(request).await;
    }
    let headers = request.headers();
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .and_then(|host| Url::parse(&format!("http://{host}")).ok());
    if host.as_ref().and_then(Url::host_str) != service.public.host_str() {
        return Failure::new(403, "Host not allowed.").into_response();
    }
    if headers.contains_key("origin") || headers.contains_key("sec-fetch-site") {
        return Failure::new(403, "Use the LiteLLM Remote app.").into_response();
    }
    if path != "/agent"
        && !matches!(
            headers
                .get("x-litellm-client")
                .and_then(|value| value.to_str().ok()),
            Some("ios" | "android" | "relay")
        )
    {
        return Failure::new(403, "Use the LiteLLM Remote app.").into_response();
    }
    if path.starts_with("/api/auth/") {
        let address = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|address| address.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        if let Err(error) = service.rate_limit(address) {
            return error.into_response();
        }
    }
    next.run(request).await
}

async fn discovery(State(service): State<Arc<Service>>) -> Json<Value> {
    Json(
        json!({ "gatewayUrl": service.gateway.as_str().trim_end_matches('/'), "controlUrl": service.public.origin().ascii_serialization() }),
    )
}

async fn identity(State(service): State<Arc<Service>>, headers: HeaderMap) -> Result<Json<Value>> {
    let identity = match bearer(&headers) {
        Some(token) => service.store.identity(&token, now_ms())?,
        None => None,
    };
    Ok(Json(
        json!({ "identity": identity, "gateway": service.gateway.as_str().trim_end_matches('/') }),
    ))
}

async fn start(State(service): State<Arc<Service>>) -> Result<impl IntoResponse> {
    service.auth.start().await.map(Json)
}
async fn poll(
    State(service): State<Arc<Service>>,
    Json(input): Json<PollInput>,
) -> Result<impl IntoResponse> {
    if !(40..=100).contains(&input.flow_token.len())
        || input.team.as_ref().is_some_and(|team| team.len() > 500)
    {
        return Err(Failure::new(400, "Invalid sign-in request."));
    }
    service
        .auth
        .poll(&input.flow_token, input.team.as_deref())
        .await
        .map(Json)
}
async fn logout(State(service): State<Arc<Service>>, headers: HeaderMap) -> Result<Json<Value>> {
    let (_, token) = service.identity(&headers)?;
    service.store.logout(&token)?;
    Ok(Json(json!({"signedOut":true})))
}

async fn devices(State(service): State<Arc<Service>>, headers: HeaderMap) -> Result<Json<Value>> {
    let (identity, _) = service.identity(&headers)?;
    Ok(Json(
        json!({ "devices": service.store.devices(&identity.subject)?.into_iter().map(|device| json!({
        "id": device.id, "name": device.name, "lastSeen": device.last_seen, "online": service.peers.online(device.id), "platform":"macOS"
    })).collect::<Vec<_>>() }),
    ))
}

async fn enroll(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    Json(input): Json<EnrollInput>,
) -> Result<Json<Value>> {
    let (identity, _) = service.identity(&headers)?;
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(Failure::new(
            400,
            "Enter a device name up to 100 characters.",
        ));
    }
    if service.store.devices(&identity.subject)?.len() >= 20 {
        return Err(Failure::new(
            429,
            "Remove an old device before adding another.",
        ));
    }
    let (device, token) = service.store.enroll(&identity.subject, name, now_ms())?;
    Ok(Json(
        json!({"deviceId":device.id,"deviceToken":token,"gateway":service.gateway.as_str().trim_end_matches('/')}),
    ))
}

async fn remove(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<Uuid>,
) -> Result<Json<Value>> {
    let (identity, _) = service.identity(&headers)?;
    service.store.device(id, &identity.subject)?;
    service.store.revoke(id, &identity.subject)?;
    service.peers.disconnect(id);
    Ok(Json(json!({"removed":true})))
}

async fn models(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<Uuid>,
) -> Result<Json<Value>> {
    let (identity, token) = service.identity(&headers)?;
    service.store.device(id, &identity.subject)?;
    let models = service.store.models(&token)?;
    if models.is_empty() {
        return Err(Failure::new(
            502,
            "The gateway did not return any models. Sign in again to refresh model access.",
        ));
    }
    Ok(Json(
        json!({ "models": models.into_iter().map(|id| json!({"id":id,"label":id,"isDefault":false})).collect::<Vec<_>>(), "nextCursor":null }),
    ))
}

async fn sessions(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<Uuid>,
    axum::extract::Query(input): axum::extract::Query<ListInput>,
) -> Result<Json<Value>> {
    service
        .request(
            &headers,
            id,
            Action::List {
                cursor: input.cursor,
            },
        )
        .await
        .map(Json)
}

async fn create(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath(id): RoutePath<Uuid>,
    Json(input): Json<CreateInput>,
) -> Result<Json<Value>> {
    if input.cwd.is_empty() || input.cwd.len() > 4096 {
        return Err(Failure::new(400, "Choose a project folder on the Mac."));
    }
    service
        .once(
            &headers,
            id,
            input.request_id,
            Action::Create {
                cwd: input.cwd,
                agent: input.agent,
            },
        )
        .await
        .map(Json)
}

async fn detail(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath((device, id)): RoutePath<(Uuid, String)>,
) -> Result<Json<Value>> {
    split_session(&id)?;
    service
        .request(&headers, device, Action::Read { session_id: id })
        .await
        .map(Json)
}

async fn message(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath((device, id)): RoutePath<(Uuid, String)>,
    Json(input): Json<MessageInput>,
) -> Result<Json<Value>> {
    split_session(&id)?;
    if input.text.trim().is_empty() || input.text.chars().count() > 32000 {
        return Err(Failure::new(
            400,
            "Enter a message up to 32,000 characters.",
        ));
    }
    let (_, token) = service.identity(&headers)?;
    if input.model.as_ref().is_some_and(|model| {
        !service
            .store
            .models(&token)
            .unwrap_or_default()
            .contains(model)
    }) {
        return Err(Failure::new(
            403,
            "This model is not available to this signed-in account.",
        ));
    }
    service
        .once(
            &headers,
            device,
            input.request_id,
            Action::Send {
                session_id: id,
                text: input.text,
                model: input.model,
            },
        )
        .await
        .map(Json)
}

async fn interrupt(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath((device, id)): RoutePath<(Uuid, String)>,
    Json(input): Json<InterruptInput>,
) -> Result<Json<Value>> {
    split_session(&id)?;
    service
        .once(
            &headers,
            device,
            input.request_id,
            Action::Interrupt { session_id: id },
        )
        .await
        .map(Json)
}

async fn approve(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    RoutePath((device, id, approval)): RoutePath<(Uuid, String, Uuid)>,
    Json(input): Json<ApprovalInput>,
) -> Result<Json<Value>> {
    split_session(&id)?;
    service
        .request(
            &headers,
            device,
            Action::Approve {
                session_id: id,
                approval_id: approval,
                decision: input.decision,
            },
        )
        .await
        .map(Json)
}

async fn agent(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response> {
    let token =
        bearer(&headers).ok_or_else(|| Failure::new(401, "Device authentication required."))?;
    let device = service
        .store
        .authenticate_device(&token)?
        .ok_or_else(|| Failure::new(401, "Device authentication failed."))?;
    Ok(upgrade
        .max_message_size(4 * 1024 * 1024)
        .on_upgrade(move |socket| service.peers.clone().connected(socket, device))
        .into_response())
}

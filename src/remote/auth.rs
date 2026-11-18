use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use url::Url;

use super::{
    protocol::{digest, now_ms, token, Failure, Result},
    store::{Identity, Store},
};

#[derive(Clone, Deserialize)]
struct LoginFlow {
    login_id: String,
    poll_secret: String,
    user_code: String,
    expires_in: Option<u64>,
}

struct Pending {
    flow: LoginFlow,
    expires_at: i64,
    polling: bool,
}

#[derive(Deserialize)]
struct GatewayPoll {
    status: String,
    key: Option<String>,
    user_id: Option<String>,
    #[serde(default)]
    requires_team_selection: bool,
    teams: Option<Vec<String>>,
    team_details: Option<Vec<GatewayTeam>>,
}

#[derive(Deserialize)]
struct GatewayTeam {
    team_id: String,
    team_alias: Option<String>,
}

#[derive(Serialize)]
pub struct Team {
    id: String,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    flow_token: String,
    code: String,
    verification_url: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum LoginStatus {
    Pending,
    Teams { teams: Vec<Team> },
    Ready { token: String, identity: Identity },
}

pub struct Authentication {
    gateway: Url,
    client: reqwest::Client,
    store: Arc<Store>,
    flows: Mutex<HashMap<String, Pending>>,
}

pub fn http_client() -> reqwest::Client {
    crate::cert::http_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("remote HTTP client configuration")
}

pub async fn response_json<T: DeserializeOwned>(mut response: reqwest::Response) -> Result<T> {
    if !response.status().is_success() {
        return Err(Failure::new(
            502,
            format!(
                "The gateway refused the request (HTTP {}).",
                response.status().as_u16()
            ),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Failure::new(502, "Cannot read the gateway response."))?
    {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err(Failure::new(502, "The gateway response is too large."));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Failure::new(502, "The gateway returned an unsupported response."))
}

impl Authentication {
    pub fn new(gateway: Url, store: Arc<Store>) -> Self {
        Self {
            gateway,
            client: http_client(),
            store,
            flows: Mutex::new(HashMap::new()),
        }
    }

    pub async fn start(&self) -> Result<LoginStart> {
        {
            let mut flows = self
                .flows
                .lock()
                .map_err(|_| Failure::new(503, "Sign-in is unavailable."))?;
            flows.retain(|_, pending| pending.expires_at > now_ms());
            if flows.len() >= 200 {
                return Err(Failure::new(
                    429,
                    "Too many sign-in attempts. Wait a minute.",
                ));
            }
        }
        let flow: LoginFlow = response_json(
            self.client
                .post(format!(
                    "{}/sso/cli/start",
                    self.gateway.as_str().trim_end_matches('/')
                ))
                .send()
                .await
                .map_err(|_| {
                    Failure::new(
                        502,
                        "Cannot reach your LiteLLM gateway. Check your network or VPN.",
                    )
                })?,
        )
        .await?;
        if flow.login_id.is_empty() || flow.poll_secret.is_empty() || flow.user_code.is_empty() {
            return Err(Failure::new(
                502,
                "The gateway did not return a sign-in challenge.",
            ));
        }
        let token = token();
        let mut verification = Url::parse(&format!(
            "{}/sso/key/generate",
            self.gateway.as_str().trim_end_matches('/')
        ))
        .map_err(|_| Failure::new(500, "The gateway address is invalid."))?;
        verification
            .query_pairs_mut()
            .append_pair("source", "litellm-cli")
            .append_pair("key", &flow.login_id);
        let started = LoginStart {
            flow_token: token.clone(),
            code: flow.user_code.clone(),
            verification_url: verification.to_string(),
        };
        self.flows
            .lock()
            .map_err(|_| Failure::new(503, "Sign-in is unavailable."))?
            .insert(
                digest(&token),
                Pending {
                    expires_at: now_ms() + flow.expires_in.unwrap_or(600).min(600) as i64 * 1000,
                    flow,
                    polling: false,
                },
            );
        Ok(started)
    }

    pub async fn poll(&self, token: &str, team: Option<&str>) -> Result<LoginStatus> {
        let key = digest(token);
        let flow = {
            let mut flows = self
                .flows
                .lock()
                .map_err(|_| Failure::new(503, "Sign-in is unavailable."))?;
            let pending = flows
                .get_mut(&key)
                .filter(|pending| pending.expires_at > now_ms())
                .ok_or_else(|| Failure::new(410, "Sign-in expired. Start again."))?;
            if pending.polling {
                return Err(Failure::new(409, "Sign-in is already being checked."));
            }
            pending.polling = true;
            pending.flow.clone()
        };
        let result = self.poll_gateway(flow, team).await;
        if let Ok(mut flows) = self.flows.lock() {
            if matches!(result, Ok(LoginStatus::Ready { .. })) {
                flows.remove(&key);
            } else if let Some(pending) = flows.get_mut(&key) {
                pending.polling = false;
            }
        }
        result
    }

    async fn poll_gateway(&self, flow: LoginFlow, team: Option<&str>) -> Result<LoginStatus> {
        let mut endpoint = Url::parse(&format!(
            "{}/sso/cli/poll/",
            self.gateway.as_str().trim_end_matches('/')
        ))
        .map_err(|_| Failure::new(500, "The gateway address is invalid."))?;
        endpoint
            .path_segments_mut()
            .map_err(|_| Failure::new(500, "The gateway address is invalid."))?
            .pop_if_empty()
            .push(&flow.login_id);
        if let Some(team) = team {
            endpoint.query_pairs_mut().append_pair("team_id", team);
        }
        let poll: GatewayPoll = response_json(
            self.client
                .get(endpoint)
                .header("x-litellm-cli-poll-secret", &flow.poll_secret)
                .send()
                .await
                .map_err(|_| Failure::new(502, "Cannot reach your LiteLLM gateway."))?,
        )
        .await?;
        if poll.status != "ready" {
            return Ok(LoginStatus::Pending);
        }
        if poll.requires_team_selection {
            let teams = poll.team_details.unwrap_or_else(|| {
                poll.teams
                    .unwrap_or_default()
                    .into_iter()
                    .map(|id| GatewayTeam {
                        team_alias: None,
                        team_id: id,
                    })
                    .collect()
            });
            return Ok(LoginStatus::Teams {
                teams: teams
                    .into_iter()
                    .map(|team| Team {
                        name: team
                            .team_alias
                            .filter(|alias| !alias.is_empty())
                            .unwrap_or_else(|| team.team_id.clone()),
                        id: team.team_id,
                    })
                    .collect(),
            });
        }
        let subject = poll
            .user_id
            .filter(|id| !id.is_empty() && id.len() <= 500)
            .ok_or_else(|| Failure::new(502, "The gateway did not return an account."))?;
        let credential = poll
            .key
            .filter(|key| !key.is_empty())
            .ok_or_else(|| Failure::new(502, "The gateway did not return a credential."))?;
        #[derive(Deserialize)]
        struct Models {
            data: Vec<Model>,
        }
        #[derive(Deserialize)]
        struct Model {
            id: String,
        }
        let catalog = self
            .client
            .get(format!(
                "{}/v1/models",
                self.gateway.as_str().trim_end_matches('/')
            ))
            .bearer_auth(&credential)
            .send()
            .await;
        let models: Vec<String> = match catalog {
            Ok(response) => response_json::<Models>(response)
                .await
                .map(|models| {
                    models
                        .data
                        .into_iter()
                        .map(|model| model.id)
                        .filter(|id| !id.is_empty() && id.len() <= 200)
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        let (token, identity) = self.store.issue(&subject, &models, now_ms())?;
        Ok(LoginStatus::Ready { token, identity })
    }
}

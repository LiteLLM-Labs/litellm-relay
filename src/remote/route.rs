use serde::Deserialize;
use url::Url;

use super::{
    auth::{http_client, response_json},
    protocol::{Failure, Result},
};

pub fn normalized(url: &Url) -> &str {
    let endpoint = url.as_str().trim_end_matches('/');
    endpoint.strip_suffix("/v1").unwrap_or(endpoint)
}

pub async fn verify(endpoint: &Url, gateway: &Url) -> Result<bool> {
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(Failure::new(
            409,
            "The inference endpoint must not contain credentials, a query, or a fragment.",
        ));
    }
    if normalized(endpoint) == normalized(gateway) {
        return Ok(false);
    }
    if endpoint.scheme() != "http"
        || !matches!(
            endpoint.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        )
        || !matches!(endpoint.path().trim_end_matches('/'), "" | "/v1")
    {
        return Err(Failure::new(
            409,
            "This agent is configured for a different gateway.",
        ));
    }
    #[derive(Deserialize)]
    struct Broker {
        gateway_url: Url,
    }
    #[derive(Deserialize)]
    struct Status {
        runtime: String,
        gateway_url: Url,
        broker: Option<Broker>,
    }
    let status: Status = response_json(
        http_client()
            .get(
                endpoint
                    .join("/api/status?passive=1")
                    .map_err(|_| Failure::new(409, "Invalid Relay endpoint."))?,
            )
            .send()
            .await
            .map_err(|_| {
                Failure::new(
                    503,
                    "Cannot reach the local Relay app. Keep Relay running on this Mac.",
                )
            })?,
    )
    .await?;
    if status.runtime != "rust" {
        return Err(Failure::new(
            409,
            "The local endpoint did not identify itself as LiteLLM Relay.",
        ));
    }
    let effective = status
        .broker
        .map(|broker| broker.gateway_url)
        .unwrap_or(status.gateway_url);
    if normalized(&effective) != normalized(gateway) {
        return Err(Failure::new(
            409,
            "Relay is using a different gateway. Switch back in Relay before continuing.",
        ));
    }
    Ok(true)
}

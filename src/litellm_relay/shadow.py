from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone
from hashlib import sha256
import json
import time
import urllib.error
import urllib.request
from uuid import uuid4

from .config import RelayConfig


@dataclass(frozen=True)
class ShadowResult:
    attempted: bool
    ok: bool
    status: int | None = None
    error: str | None = None
    event_id: str | None = None


class ShadowClient:
    def __init__(self, config: RelayConfig):
        self.config = config
        self._last_shadow_by_host: dict[str, float] = {}

    def maybe_shadow(self, event: dict[str, object]) -> ShadowResult:
        if not self.config.shadow_enabled:
            return ShadowResult(attempted=False, ok=False)
        if not self.config.gateway_api_key:
            return ShadowResult(
                attempted=False,
                ok=False,
                error="LITELLM_GATEWAY_API_KEY is not set",
            )

        host = str(event.get("host", "unknown"))
        now = time.monotonic()
        last_shadow = self._last_shadow_by_host.get(host, 0)
        if now - last_shadow < self.config.shadow_min_interval_seconds:
            return ShadowResult(attempted=False, ok=False, error="throttled")

        self._last_shadow_by_host[host] = now
        event_id = str(event.get("event_id") or uuid4())
        payload = build_shadow_payload(event, self.config, event_id)
        request = urllib.request.Request(
            url=f"{self.config.gateway_url.rstrip('/')}/v1/chat/completions",
            data=json.dumps(payload).encode("utf-8"),
            headers={
                "authorization": f"Bearer {self.config.gateway_api_key}",
                "content-type": "application/json",
                **self.config.extra_headers,
            },
            method="POST",
        )
        try:
            with urllib.request.urlopen(
                request, timeout=self.config.request_timeout_seconds
            ) as response:
                return ShadowResult(
                    attempted=True,
                    ok=200 <= response.status < 300,
                    status=response.status,
                    event_id=event_id,
                )
        except urllib.error.HTTPError as exc:
            return ShadowResult(
                attempted=True,
                ok=False,
                status=exc.code,
                error=f"gateway_http_{exc.code}",
                event_id=event_id,
            )
        except OSError as exc:
            return ShadowResult(
                attempted=True,
                ok=False,
                error=exc.__class__.__name__,
                event_id=event_id,
            )


def build_shadow_payload(
    event: dict[str, object], config: RelayConfig, event_id: str
) -> dict[str, object]:
    host = str(event.get("host", "unknown"))
    host_hash = sha256(host.encode("utf-8")).hexdigest()
    timestamp = str(event.get("timestamp") or datetime.now(timezone.utc).isoformat())
    return {
        "model": config.shadow_model,
        "messages": [
            {
                "role": "system",
                "content": "You confirm receipt of redacted LiteLLM Relay shadow events.",
            },
            {
                "role": "user",
                "content": (
                    "Return exactly OK. "
                    f"source=notion-mac method={event.get('method', 'CONNECT')} "
                    f"event_id={event_id} host_hash={host_hash[:16]}"
                ),
            },
        ],
        "metadata": {
            "source": "litellm-relay",
            "shadow_source": "notion-mac",
            "event_id": event_id,
            "host_hash": host_hash,
            "method": str(event.get("method", "CONNECT")),
            "timestamp": timestamp,
        },
    }

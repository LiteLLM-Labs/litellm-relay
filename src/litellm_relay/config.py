from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
import os


DEFAULT_NOTION_DOMAINS = (
    "notion.so",
    "notion.com",
    "api.notion.com",
    "www.notion.so",
    "app.notion.com",
)

DEFAULT_AI_DOMAINS = (
    *DEFAULT_NOTION_DOMAINS,
    "api.openai.com",
    "openai.com",
    "chatgpt.com",
    "api.anthropic.com",
    "anthropic.com",
    "claude.ai",
)


@dataclass(frozen=True)
class RelayConfig:
    host: str = "127.0.0.1"
    port: int = 4142
    log_path: Path = Path.home() / ".litellm-relay" / "relay.log.jsonl"
    notion_domains: tuple[str, ...] = DEFAULT_NOTION_DOMAINS
    ai_domains: tuple[str, ...] = DEFAULT_AI_DOMAINS
    shadow_enabled: bool = False
    gateway_url: str = "http://127.0.0.1:4000"
    gateway_api_key: str | None = None
    shadow_model: str = "gpt-4o-mini"
    shadow_min_interval_seconds: int = 60
    request_timeout_seconds: float = 10.0
    extra_headers: dict[str, str] = field(default_factory=dict)

    @classmethod
    def from_env(cls) -> "RelayConfig":
        notion_domains = parse_domains(
            os.getenv("LITELLM_RELAY_NOTION_DOMAINS"), DEFAULT_NOTION_DOMAINS
        )
        ai_domains = parse_domains(
            os.getenv("LITELLM_RELAY_AI_DOMAINS"), DEFAULT_AI_DOMAINS
        )
        return cls(
            host=os.getenv("LITELLM_RELAY_HOST", "127.0.0.1"),
            port=int(os.getenv("LITELLM_RELAY_PORT", "4142")),
            log_path=Path(
                os.getenv(
                    "LITELLM_RELAY_LOG_PATH",
                    str(Path.home() / ".litellm-relay" / "relay.log.jsonl"),
                )
            ),
            notion_domains=notion_domains,
            ai_domains=ai_domains,
            shadow_enabled=os.getenv("LITELLM_RELAY_SHADOW_ENABLED", "").lower()
            in {"1", "true", "yes", "on"},
            gateway_url=os.getenv("LITELLM_GATEWAY_URL", "http://127.0.0.1:4000"),
            gateway_api_key=os.getenv("LITELLM_GATEWAY_API_KEY"),
            shadow_model=os.getenv("LITELLM_RELAY_SHADOW_MODEL", "gpt-4o-mini"),
            shadow_min_interval_seconds=int(
                os.getenv("LITELLM_RELAY_SHADOW_MIN_INTERVAL_SECONDS", "60")
            ),
            request_timeout_seconds=float(
                os.getenv("LITELLM_RELAY_REQUEST_TIMEOUT_SECONDS", "10")
            ),
        )


def parse_domains(raw: str | None, default: tuple[str, ...]) -> tuple[str, ...]:
    if not raw:
        return default
    return tuple(d.strip().lower() for d in raw.split(",") if d.strip())


def normalize_host(host: str) -> str:
    host = host.strip().lower()
    if host.startswith("["):
        return host.split("]", 1)[0].strip("[]")
    return host.split(":", 1)[0]


def is_domain_match(host: str, domains: tuple[str, ...]) -> bool:
    normalized = normalize_host(host)
    return any(normalized == domain or normalized.endswith(f".{domain}") for domain in domains)


def is_notion_host(host: str, config: RelayConfig) -> bool:
    return is_domain_match(host, config.notion_domains)


def is_ai_host(host: str, config: RelayConfig) -> bool:
    return is_domain_match(host, config.ai_domains)


def classify_host(host: str, config: RelayConfig) -> str:
    normalized = normalize_host(host)
    if is_domain_match(normalized, config.notion_domains):
        return "notion"
    if is_domain_match(normalized, ("api.openai.com", "openai.com", "chatgpt.com")):
        return "openai"
    if is_domain_match(normalized, ("api.anthropic.com", "anthropic.com", "claude.ai")):
        return "anthropic"
    if is_ai_host(normalized, config):
        return "ai"
    return "unknown"

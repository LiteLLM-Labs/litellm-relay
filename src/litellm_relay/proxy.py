from __future__ import annotations

import asyncio
from collections import deque
from datetime import datetime, timezone
import json
import time
from urllib.parse import parse_qs, urlsplit
from uuid import uuid4

from .config import RelayConfig, classify_host, is_ai_host, is_notion_host, normalize_host
from .pac import build_pac
from .shadow import ShadowClient
from .ui import DASHBOARD_HTML


class RelayProxy:
    def __init__(self, config: RelayConfig):
        self.config = config
        self.shadow_client = ShadowClient(config)

    async def serve_forever(self) -> None:
        self.config.log_path.parent.mkdir(parents=True, exist_ok=True)
        server = await asyncio.start_server(
            self._handle_client, self.config.host, self.config.port
        )
        self._log(
            {
                "event": "relay_started",
                "listen": f"{self.config.host}:{self.config.port}",
                "shadow_enabled": self.config.shadow_enabled,
            }
        )
        async with server:
            await server.serve_forever()

    async def _handle_client(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        peer = writer.get_extra_info("peername")
        try:
            header = await reader.readuntil(b"\r\n\r\n")
        except asyncio.IncompleteReadError:
            writer.close()
            await writer.wait_closed()
            return

        try:
            header_text = header.decode("iso-8859-1")
            request_line = header_text.splitlines()[0]
            method, target, _version = request_line.split(" ", 2)
        except (UnicodeDecodeError, ValueError, IndexError):
            await self._write_response(writer, 400, b"bad request\n")
            return

        method = method.upper()
        route = parse_route(target)

        if method in {"GET", "HEAD"} and route.path in {"/", "/index.html"}:
            await self._write_response(
                writer,
                200,
                DASHBOARD_HTML.encode("utf-8"),
                content_type="text/html; charset=utf-8",
                include_body=method == "GET",
            )
            return

        if method in {"GET", "HEAD"} and route.path in {"/proxy.pac", "/pac"}:
            pac = build_pac(self.config).encode("utf-8")
            await self._write_response(
                writer,
                200,
                pac,
                content_type="application/x-ns-proxy-autoconfig",
                include_body=method == "GET",
            )
            return

        if method == "GET" and route.path == "/api/status":
            await self._write_json(writer, self._status_payload())
            return

        if method == "GET" and route.path == "/api/events":
            limit = parse_limit(route.query)
            await self._write_json(
                writer,
                {
                    "events": read_events(self.config.log_path, limit=limit),
                    "limit": limit,
                },
            )
            return

        if method == "POST" and route.path == "/api/events/clear":
            self.config.log_path.parent.mkdir(parents=True, exist_ok=True)
            self.config.log_path.write_text("", encoding="utf-8")
            self._log(
                {
                    "event": "relay_log_cleared",
                    "listen": f"{self.config.host}:{self.config.port}",
                }
            )
            await self._write_json(writer, {"ok": True})
            return

        if method == "CONNECT":
            await self._handle_connect(target, peer, reader, writer)
            return

        await self._write_response(
            writer,
            501,
            b"litellm-relay v0 only supports CONNECT tunneling and /proxy.pac\n",
        )

    async def _handle_connect(
        self,
        target: str,
        peer: object,
        client_reader: asyncio.StreamReader,
        client_writer: asyncio.StreamWriter,
    ) -> None:
        host, port = parse_connect_target(target)
        started_at = time.monotonic()
        event_id = str(uuid4())
        event = self._event(
            {
                "event_id": event_id,
                "event": "connect",
                "method": "CONNECT",
                "host": host,
                "port": port,
                "peer": repr(peer),
                "app": classify_host(host, self.config),
                "ai_match": is_ai_host(host, self.config),
                "notion_match": is_notion_host(host, self.config),
            }
        )

        if event["ai_match"]:
            shadow_result = self.shadow_client.maybe_shadow(event)
            event["shadow"] = {
                "attempted": shadow_result.attempted,
                "ok": shadow_result.ok,
                "status": shadow_result.status,
                "error": shadow_result.error,
                "event_id": shadow_result.event_id,
            }
        self._log(event)

        try:
            upstream_reader, upstream_writer = await asyncio.open_connection(host, port)
        except OSError as exc:
            self._log(
                self._event(
                    {
                        "event": "connect_failed",
                        "host": host,
                        "port": port,
                        "error": exc.__class__.__name__,
                    }
                )
            )
            await self._write_response(client_writer, 502, b"upstream connect failed\n")
            return

        client_writer.write(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        await client_writer.drain()
        bytes_out, bytes_in = await tunnel(
            client_reader, client_writer, upstream_reader, upstream_writer
        )
        duration_ms = int((time.monotonic() - started_at) * 1000)
        self._log(
            self._event(
                {
                    "event_id": event_id,
                    "event": "connect_closed",
                    "method": "CONNECT",
                    "host": host,
                    "port": port,
                    "app": classify_host(host, self.config),
                    "ai_match": is_ai_host(host, self.config),
                    "notion_match": is_notion_host(host, self.config),
                    "duration_ms": duration_ms,
                    "bytes_out": bytes_out,
                    "bytes_in": bytes_in,
                }
            )
        )

    async def _write_response(
        self,
        writer: asyncio.StreamWriter,
        status: int,
        body: bytes,
        content_type: str = "text/plain",
        include_body: bool = True,
    ) -> None:
        reason = {
            200: "OK",
            400: "Bad Request",
            501: "Not Implemented",
            502: "Bad Gateway",
        }.get(status, "OK")
        writer.write(
            (
                f"HTTP/1.1 {status} {reason}\r\n"
                f"content-length: {len(body)}\r\n"
                f"content-type: {content_type}\r\n"
                "cache-control: no-store\r\n"
                "connection: close\r\n"
                "\r\n"
            ).encode("ascii")
            + (body if include_body else b"")
        )
        await writer.drain()
        writer.close()
        await writer.wait_closed()

    async def _write_json(
        self, writer: asyncio.StreamWriter, payload: dict[str, object], status: int = 200
    ) -> None:
        await self._write_response(
            writer,
            status,
            json.dumps(payload, sort_keys=True).encode("utf-8"),
            content_type="application/json; charset=utf-8",
        )

    def _event(self, values: dict[str, object]) -> dict[str, object]:
        return {
            "event_id": str(uuid4()),
            "timestamp": datetime.now(timezone.utc).isoformat(),
            **values,
        }

    def _log(self, event: dict[str, object]) -> None:
        self.config.log_path.parent.mkdir(parents=True, exist_ok=True)
        with self.config.log_path.open("a", encoding="utf-8") as log_file:
            log_file.write(json.dumps(redact_event(event), sort_keys=True) + "\n")

    def _status_payload(self) -> dict[str, object]:
        return {
            "listen": f"{self.config.host}:{self.config.port}",
            "log_path": str(self.config.log_path),
            "ai_domains": list(self.config.ai_domains),
            "notion_domains": list(self.config.notion_domains),
            "shadow_enabled": self.config.shadow_enabled,
            "gateway_url": self.config.gateway_url,
            "events_loaded": len(read_events(self.config.log_path, limit=1000)),
        }


async def tunnel(
    client_reader: asyncio.StreamReader,
    client_writer: asyncio.StreamWriter,
    upstream_reader: asyncio.StreamReader,
    upstream_writer: asyncio.StreamWriter,
) -> tuple[int, int]:
    async def pipe(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> int:
        byte_count = 0
        try:
            while chunk := await reader.read(65536):
                byte_count += len(chunk)
                writer.write(chunk)
                await writer.drain()
        except (ConnectionError, asyncio.CancelledError):
            pass
        finally:
            writer.close()
        return byte_count

    results = await asyncio.gather(
        pipe(client_reader, upstream_writer),
        pipe(upstream_reader, client_writer),
        return_exceptions=True,
    )
    bytes_out = results[0] if isinstance(results[0], int) else 0
    bytes_in = results[1] if isinstance(results[1], int) else 0
    return bytes_out, bytes_in


class Route:
    def __init__(self, path: str, query: str):
        self.path = path
        self.query = query


def parse_route(target: str) -> Route:
    parsed = urlsplit(target)
    path = parsed.path or "/"
    return Route(path=path, query=parsed.query)


def parse_limit(query: str) -> int:
    raw = parse_qs(query).get("limit", ["250"])[0]
    try:
        return max(1, min(int(raw), 1000))
    except ValueError:
        return 250


def read_events(log_path, limit: int = 250) -> list[dict[str, object]]:
    if not log_path.exists():
        return []
    events: deque[dict[str, object]] = deque(maxlen=limit)
    with log_path.open("r", encoding="utf-8") as log_file:
        for line in log_file:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(event, dict):
                events.append(event)
    return list(events)


def parse_connect_target(target: str) -> tuple[str, int]:
    if target.startswith("["):
        host, _, rest = target[1:].partition("]")
        port = int(rest.lstrip(":") or "443")
        return host, port
    host, sep, port = target.partition(":")
    return normalize_host(host), int(port) if sep else 443


def redact_event(event: dict[str, object]) -> dict[str, object]:
    redacted = dict(event)
    for key in ("authorization", "cookie", "token_v2", "prompt", "body", "url"):
        redacted.pop(key, None)
    return redacted

from __future__ import annotations

import asyncio
from datetime import datetime, timezone
import json
from uuid import uuid4

from .config import RelayConfig, is_notion_host, normalize_host
from .pac import build_pac
from .shadow import ShadowClient


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

        if method.upper() == "GET" and target in {"/proxy.pac", "/pac"}:
            pac = build_pac(self.config).encode("utf-8")
            await self._write_response(
                writer, 200, pac, content_type="application/x-ns-proxy-autoconfig"
            )
            return

        if method.upper() == "CONNECT":
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
        event = self._event(
            {
                "event": "connect",
                "method": "CONNECT",
                "host": host,
                "port": port,
                "peer": repr(peer),
                "notion_match": is_notion_host(host, self.config),
            }
        )

        if event["notion_match"]:
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
        await tunnel(client_reader, client_writer, upstream_reader, upstream_writer)

    async def _write_response(
        self,
        writer: asyncio.StreamWriter,
        status: int,
        body: bytes,
        content_type: str = "text/plain",
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
                "connection: close\r\n"
                "\r\n"
            ).encode("ascii")
            + body
        )
        await writer.drain()
        writer.close()
        await writer.wait_closed()

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


async def tunnel(
    client_reader: asyncio.StreamReader,
    client_writer: asyncio.StreamWriter,
    upstream_reader: asyncio.StreamReader,
    upstream_writer: asyncio.StreamWriter,
) -> None:
    async def pipe(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            while chunk := await reader.read(65536):
                writer.write(chunk)
                await writer.drain()
        except (ConnectionError, asyncio.CancelledError):
            pass
        finally:
            writer.close()

    await asyncio.gather(
        pipe(client_reader, upstream_writer),
        pipe(upstream_reader, client_writer),
        return_exceptions=True,
    )


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

import unittest

from pathlib import Path
import tempfile

from litellm_relay.proxy import parse_connect_target, parse_limit, parse_route, read_events, redact_event


class ProxyTests(unittest.TestCase):
    def test_parse_connect_target_defaults_to_tls_port(self):
        self.assertEqual(parse_connect_target("www.notion.so"), ("www.notion.so", 443))


    def test_parse_connect_target_reads_port(self):
        self.assertEqual(parse_connect_target("www.notion.so:443"), ("www.notion.so", 443))

    def test_parse_route_reads_path_and_query(self):
        route = parse_route("/api/events?limit=50")
        self.assertEqual(route.path, "/api/events")
        self.assertEqual(route.query, "limit=50")

    def test_parse_limit_clamps_invalid_values(self):
        self.assertEqual(parse_limit("limit=2"), 2)
        self.assertEqual(parse_limit("limit=999999"), 1000)
        self.assertEqual(parse_limit("limit=nope"), 250)

    def test_read_events_skips_bad_json_and_limits_rows(self):
        with tempfile.TemporaryDirectory() as tmpdir:
            log_path = Path(tmpdir) / "relay.log.jsonl"
            log_path.write_text(
                '{"event":"one"}\nnot-json\n{"event":"two"}\n',
                encoding="utf-8",
            )
            self.assertEqual(read_events(log_path, limit=1), [{"event": "two"}])


    def test_redact_event_drops_sensitive_fields(self):
        redacted = redact_event(
            {
                "event": "connect",
                "host": "www.notion.so",
                "cookie": "token_v2=secret",
                "authorization": "Bearer secret",
                "body": "prompt",
            }
        )
        self.assertNotIn("cookie", redacted)
        self.assertNotIn("authorization", redacted)
        self.assertNotIn("body", redacted)
        self.assertEqual(redacted["host"], "www.notion.so")


if __name__ == "__main__":
    unittest.main()

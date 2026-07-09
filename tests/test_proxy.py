import unittest

from litellm_relay.proxy import parse_connect_target, redact_event


class ProxyTests(unittest.TestCase):
    def test_parse_connect_target_defaults_to_tls_port(self):
        self.assertEqual(parse_connect_target("www.notion.so"), ("www.notion.so", 443))


    def test_parse_connect_target_reads_port(self):
        self.assertEqual(parse_connect_target("www.notion.so:443"), ("www.notion.so", 443))


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

import unittest

from litellm_relay.config import RelayConfig
from litellm_relay.shadow import build_shadow_payload


class ShadowTests(unittest.TestCase):
    def test_shadow_payload_excludes_raw_host_and_uses_hash(self):
        payload = build_shadow_payload(
            {"host": "www.notion.so", "method": "CONNECT"},
            RelayConfig(shadow_model="test-model"),
            "event-1",
        )

        serialized = str(payload)
        self.assertEqual(payload["model"], "test-model")
        self.assertEqual(payload["metadata"]["event_id"], "event-1")
        self.assertNotIn("www.notion.so", serialized)
        self.assertIn("host_hash", payload["metadata"])


if __name__ == "__main__":
    unittest.main()

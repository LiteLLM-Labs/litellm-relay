import unittest

from litellm_relay.config import RelayConfig
from litellm_relay.pac import build_pac


class PacTests(unittest.TestCase):
    def test_build_pac_routes_only_notion_domains(self):
        pac = build_pac(RelayConfig(port=4142))
        self.assertIn("PROXY 127.0.0.1:4142", pac)
        self.assertIn('"notion.so"', pac)
        self.assertIn('return "DIRECT"', pac)


if __name__ == "__main__":
    unittest.main()

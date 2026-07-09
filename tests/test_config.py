import unittest

from litellm_relay.config import RelayConfig, is_notion_host, normalize_host


class ConfigTests(unittest.TestCase):
    def test_normalize_host_strips_port(self):
        self.assertEqual(normalize_host("www.notion.so:443"), "www.notion.so")


    def test_is_notion_host_matches_subdomains(self):
        config = RelayConfig()
        self.assertTrue(is_notion_host("www.notion.so:443", config))
        self.assertTrue(is_notion_host("app.notion.com", config))
        self.assertFalse(is_notion_host("example.com", config))


if __name__ == "__main__":
    unittest.main()

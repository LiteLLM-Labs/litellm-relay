import unittest

from litellm_relay.config import RelayConfig, classify_host, is_ai_host, is_notion_host, normalize_host


class ConfigTests(unittest.TestCase):
    def test_normalize_host_strips_port(self):
        self.assertEqual(normalize_host("www.notion.so:443"), "www.notion.so")


    def test_is_notion_host_matches_subdomains(self):
        config = RelayConfig()
        self.assertTrue(is_notion_host("www.notion.so:443", config))
        self.assertTrue(is_notion_host("app.notion.com", config))
        self.assertFalse(is_notion_host("example.com", config))

    def test_is_ai_host_matches_openai_and_notion(self):
        config = RelayConfig()
        self.assertTrue(is_ai_host("api.openai.com:443", config))
        self.assertTrue(is_ai_host("www.notion.so", config))
        self.assertFalse(is_ai_host("example.com", config))

    def test_classify_host_returns_known_apps(self):
        config = RelayConfig()
        self.assertEqual(classify_host("api.openai.com", config), "openai")
        self.assertEqual(classify_host("api.anthropic.com", config), "anthropic")
        self.assertEqual(classify_host("www.notion.so", config), "notion")


if __name__ == "__main__":
    unittest.main()

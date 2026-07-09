from __future__ import annotations

from .config import RelayConfig


def build_pac(config: RelayConfig) -> str:
    domains = ",\n    ".join(f'"{domain}"' for domain in config.notion_domains)
    return f"""function FindProxyForURL(url, host) {{
  var relayProxy = "PROXY {config.host}:{config.port}";
  var notionDomains = [
    {domains}
  ];

  host = host.toLowerCase();
  for (var i = 0; i < notionDomains.length; i++) {{
    var domain = notionDomains[i];
    if (host === domain || dnsDomainIs(host, "." + domain)) {{
      return relayProxy;
    }}
  }}

  return "DIRECT";
}}
"""


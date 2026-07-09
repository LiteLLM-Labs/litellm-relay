use crate::config::RelayConfig;

pub fn build_pac(config: &RelayConfig) -> String {
    let domains = config
        .ai_domains
        .iter()
        .map(|domain| format!("    \"{domain}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "function FindProxyForURL(url, host) {{\n  var relayProxy = \"PROXY {}:{}\";\n  var notionDomains = [\n{}\n  ];\n\n  host = host.toLowerCase();\n  for (var i = 0; i < notionDomains.length; i++) {{\n    var domain = notionDomains[i];\n    if (host === domain || dnsDomainIs(host, \".\" + domain)) {{\n      return relayProxy;\n    }}\n  }}\n\n  return \"DIRECT\";\n}}\n",
        config.host, config.port, domains
    )
}

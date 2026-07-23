class LitellmRelay < Formula
  desc "Local relay for routing and auditing AI app traffic through LiteLLM Gateway"
  homepage "https://github.com/LiteLLM-Labs/litellm-relay"
  # Relay has no tagged release yet, so the stable spec pins a specific commit
  # for a reproducible `brew install`. Once v0.1.0 is tagged, replace this with
  # the release source tarball and its sha256, e.g.:
  #   url "https://github.com/LiteLLM-Labs/litellm-relay/archive/refs/tags/v0.1.0.tar.gz"
  #   sha256 "<tarball-sha256>"
  url "https://github.com/LiteLLM-Labs/litellm-relay.git",
      revision: "2a6a7a8be561845a11ff69b5ae55f83166172ef4"
  version "0.1.0"
  license "Apache-2.0"
  head "https://github.com/LiteLLM-Labs/litellm-relay.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
    # install.sh exposes the binary under both names; mirror the `relay` shim.
    bin.install_symlink bin/"litellm-relay" => "relay"
  end

  def caveats
    <<~EOS
      Homebrew installs only the `relay` (and `litellm-relay`) command. Unlike
      the bundled install.sh, it does NOT trust the local Relay CA, start the
      background LaunchAgent, or change your system proxy.

      Finish setup and open the live trace view:
        relay

      Trust the Relay CA so AI app payloads can be captured (macOS):
        security add-trusted-cert -r trustRoot \\
          -k "$HOME/Library/Keychains/login.keychain-db" "$(relay ca-path)"

      Background service, system proxy, and MDM rollout instructions:
        https://github.com/LiteLLM-Labs/litellm-relay
    EOS
  end

  test do
    assert_match "Gateway relay", shell_output("#{bin}/relay --help")
    assert_path_exists bin/"litellm-relay"
  end
end

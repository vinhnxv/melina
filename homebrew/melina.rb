class Melina < Formula
  desc "Claude Code process monitor — track sessions, teammates, MCP servers, and orphans"
  homepage "https://github.com/vinhnx/melina"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/vinhnx/melina/releases/download/v#{version}/melina-v#{version}-aarch64-apple-darwin.tar.gz"
      # sha256 "PLACEHOLDER" # Updated automatically by release workflow
    elsif Hardware::CPU.intel?
      url "https://github.com/vinhnx/melina/releases/download/v#{version}/melina-v#{version}-x86_64-apple-darwin.tar.gz"
      # sha256 "PLACEHOLDER"
    end
  end

  on_linux do
    url "https://github.com/vinhnx/melina/releases/download/v#{version}/melina-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
    # sha256 "PLACEHOLDER"
  end

  def install
    bin.install "melina"
    bin.install "melina-tui"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/melina --version", 2)
  end
end

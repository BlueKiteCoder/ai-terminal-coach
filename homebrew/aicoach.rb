class Aicoach < Formula
  desc "AI collaboration layer for macOS Zsh terminals"
  homepage "https://github.com/BlueKiteCoder/ai-terminal-coach"
  url "https://github.com/BlueKiteCoder/ai-terminal-coach/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "REPLACE_WITH_RELEASE_TARBALL_SHA256"
  license "MIT"
  head "https://github.com/BlueKiteCoder/ai-terminal-coach.git", branch: "main"

  depends_on :macos
  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: "crates/aicoach-cli")
    system "cargo", "install", *std_cargo_args(path: "crates/aicoach-daemon")
    system "cargo", "install", *std_cargo_args(path: "crates/aicoach-tui")

    pkgshare.install "shell/aicoach.zsh"
    pkgshare.install "config/default.toml"
    pkgshare.install "scripts/aicoach-window.js"
    pkgshare.install "scripts/aicoach-hide.js"

    # The script prefers Swift and falls back to Objective-C/AppKit when the
    # active Swift compiler and SDK do not match.
    system "scripts/build-macos-helper.sh", bin/"aicoach-hotkey"
  end

  def caveats
    <<~EOS
      Install the Zsh and LaunchAgent integration with:
        aicoach install

      Run `aicoach install` again after each Homebrew upgrade so LaunchAgent
      paths are refreshed to the current Cellar version.

      Store an API key in macOS Keychain (recommended):
        aicoach config set-key

      Then restart the daemon:
        aicoach restart

      Option+Tab invokes AI completion. Option+/ asks about the current
      buffer. Option+Space toggles the Coach window when Option sends Esc.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/aicoach --version")
    assert_match "AI Terminal Coach", shell_output("#{bin}/aicoach --help")
  end
end

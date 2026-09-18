# typed: strict
# frozen_string_literal: true

# Formula source shared by HEAD development installs and generated stable releases.
class Aicoach < Formula
  desc "Local-first AI collaboration layer for macOS Zsh terminals"
  homepage "https://github.com/BlueKiteCoder/ai-terminal-coach"
  license "MIT"
  head "https://github.com/BlueKiteCoder/ai-terminal-coach.git", branch: "main"

  depends_on "rust" => :build
  depends_on macos: :ventura

  def fetch
    system "cargo", "fetch", "--locked"
  end

  def install
    ENV["CARGO_NET_OFFLINE"] = "true"
    system "cargo", "install", *std_cargo_args(path: "crates/aicoach-cli")
    system "cargo", "install", *std_cargo_args(path: "crates/aicoach-daemon")
    system "cargo", "install", *std_cargo_args(path: "crates/aicoach-tui")

    pkgshare.install "shell/aicoach.zsh"
    pkgshare.install "config/default.toml"
    pkgshare.install "scripts/aicoach-window.js"
    pkgshare.install "scripts/aicoach-hide.js"
    prefix.install_metafiles

    # The script prefers Swift and falls back to Objective-C/AppKit when the
    # active Swift compiler and SDK do not match. Homebrew's sandbox permits
    # the staged build path, not mktemp's per-user default on every macOS.
    inreplace "scripts/build-macos-helper.sh",
              "module_cache=$(mktemp -d)",
              "module_cache=$(mktemp -d \"#{buildpath}/aicoach-hotkey.XXXXXX\")"
    system "scripts/build-macos-helper.sh", bin/"aicoach-hotkey"
  end

  def caveats
    <<~EOS
      After every install or upgrade, refresh the Zsh and LaunchAgent integration with:
        aicoach install

      Configure an optional AI provider and Keychain credential:
        aicoach config setup

      Option+Tab invokes AI completion. Option+/ asks about the current
      buffer. Option+Space toggles the Coach window when Option sends Esc.
    EOS
  end

  test do
    assert_match(/^aicoach \d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/,
                 shell_output("#{bin}/aicoach --version").strip)
    assert_match ".config/aicoach/config.toml", shell_output("#{bin}/aicoach config path")
  end
end

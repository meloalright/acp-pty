class ShellAcp < Formula
  desc "Shell exposed as an Agent Client Protocol (ACP) agent"
  homepage "https://github.com/meloalright/shell-acp"
  version "__VERSION__"

  on_macos do
    on_arm do
      url "https://github.com/meloalright/shell-acp/releases/download/v__VERSION__/shell-acp-__VERSION__-aarch64-apple-darwin.tar.gz"
      sha256 "__SHA_MAC_ARM__"
    end
    on_intel do
      url "https://github.com/meloalright/shell-acp/releases/download/v__VERSION__/shell-acp-__VERSION__-x86_64-apple-darwin.tar.gz"
      sha256 "__SHA_MAC_X86__"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/meloalright/shell-acp/releases/download/v__VERSION__/shell-acp-__VERSION__-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "__SHA_LIN_ARM__"
    end
    on_intel do
      url "https://github.com/meloalright/shell-acp/releases/download/v__VERSION__/shell-acp-__VERSION__-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "__SHA_LIN_X86__"
    end
  end

  def install
    bin.install "shell-acp"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/shell-acp --version")
  end
end

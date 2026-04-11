class Pandafilter < Formula
  desc "LLM token optimizer for Claude Code — 60-90% token savings on dev operations"
  homepage "https://github.com/AssafWoo/PandaFilter"
  license "MIT"
  version "0.6.15"

  depends_on "jq"

  # Prebuilt binaries — no Rust/LLVM build dependencies, installs in seconds.
  # Each tarball contains the panda binary + libonnxruntime dylib bundled together.
  on_arm do
    url "https://github.com/AssafWoo/PandaFilter/releases/download/v0.6.15/panda-macos-arm64.tar.gz"
    sha256 "01895624a2d3353ca975201c95962ef5a20f3bc1c217d6123d43a80dd3e75398"
  end

  on_intel do
    url "https://github.com/AssafWoo/PandaFilter/releases/download/v0.6.15/panda-macos-x86_64.tar.gz"
    sha256 "0bd5d5b000ab689a6a8ab4e55db4cf22e7f27771f728841b28b0b0d93274e388"
  end

  def install
    bin.install "panda"
    # Install the bundled ORT dylib and fix rpath so the binary finds it
    dylib = Dir["libonnxruntime*.dylib"].first
    if dylib
      lib.install dylib
      system "install_name_tool", "-add_rpath", lib.to_s, "#{bin}/panda"
    end

    # Compatibility shim bundled in the tarball — install it so old `ccr` hooks keep working
    bin.install "ccr"
  end

  def post_install
    # Pre-download the BERT model and register hooks automatically.
    # Runs as the installing user so ~/.cache, ~/.claude, and ~/.cursor are correct.
    # quiet_system — don't fail the install if an agent isn't set up yet.
    quiet_system bin/"panda", "init"
    quiet_system bin/"panda", "init", "--agent", "cursor"
  end

  def caveats
    <<~EOS
      PandaFilter setup runs automatically during install (hooks + BERT model download).
      If you see hook errors, re-run manually:
        panda init                      # Claude Code
        panda init --agent cursor       # Cursor
    EOS
  end

  test do
    assert_match "filter", shell_output("#{bin}/panda --help")
    assert_match(/\S/, pipe_output("#{bin}/panda filter", "hello world\n"))
  end
end

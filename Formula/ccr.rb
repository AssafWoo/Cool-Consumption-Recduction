class Ccr < Formula
  desc "LLM token optimizer for Claude Code — 60-90% token savings on dev operations"
  homepage "https://github.com/AssafWoo/homebrew-ccr"
  url "https://github.com/AssafWoo/homebrew-ccr/archive/refs/tags/v0.5.17.tar.gz"
  sha256 "5c209c28b460a0f0ccc7c2439c0f30c8ef9c575b1c579d635c101b43ea3f42bb"
  license "MIT"
  head "https://github.com/AssafWoo/homebrew-ccr.git", branch: "main"

  depends_on "rust" => :build

  # The `ort` crate (ONNX Runtime Rust bindings) downloads a pre-built ORT
  # binary during `cargo build`. Homebrew's sandbox blocks network access at
  # build time, so we fetch ORT here as a resource and point `ort` at it via
  # ORT_LIB_LOCATION before building.
  #
  # ORT version: 1.20.1 (required by ort = "2.0.0-rc.9")
  # To verify SHA256: shasum -a 256 <downloaded-tgz>
  on_arm do
    resource "ort-runtime" do
      url "https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-osx-arm64-1.20.1.tgz"
      sha256 "b678fc3c2354c771fea4fba420edeccfba205140088334df801e7fc40e83a57a"
    end
  end

  on_intel do
    resource "ort-runtime" do
      url "https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-osx-x86_64-1.20.1.tgz"
      sha256 "0f73006813af2a1a5d1723ed7dfb694fc629d15037124081bb61b7bf7d99fc78"
    end
  end

  def install
    # Extract ORT and expose it to the ort-sys build script
    ort_dir = buildpath/"ort-extracted"
    ort_dir.mkpath
    resource("ort-runtime").stage(ort_dir)

    # ort-sys looks for the dylib in ORT_LIB_LOCATION
    # The tarball unpacks to onnxruntime-osx-{arch}-{ver}/lib/
    ort_lib = Dir["#{ort_dir}/**/lib"].first
    ENV["ORT_LIB_LOCATION"] = ort_lib if ort_lib

    # Prevent ort-sys from attempting a network download
    ENV["ORT_STRATEGY"] = "system"

    system "cargo", "install", *std_cargo_args(path: "ccr")

    # Install the ORT dylib so it's available at runtime.
    # The binary has @rpath/libonnxruntime.*.dylib embedded; adding #{lib} to
    # its rpath lets dyld resolve it after `brew install`.
    if ort_lib
      lib.install Dir["#{ort_lib}/libonnxruntime*.dylib"]
      system "install_name_tool", "-add_rpath", lib.to_s, "#{bin}/ccr"
    end
  end

  def caveats
    <<~EOS
      Register CCR with Claude Code:
        ccr init
    EOS
  end

  test do
    # Verify the binary runs and core subcommands are present
    assert_match "filter", shell_output("#{bin}/ccr --help")
    # filter subcommand must compress stdin
    assert_match(/\S/, pipe_output("#{bin}/ccr filter", "hello world\n"))
  end
end

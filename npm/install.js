const { execSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const os = require("os");

const BIN_NAME = "shell-acp";
const REPO = "meloalright/shell-acp";
const VERSION = require("./package.json").version;

function getTarget() {
  const platform = os.platform();
  const arch = os.arch();

  if (platform === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (platform === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-musl";
  if (platform === "linux" && arch === "arm64") return "aarch64-unknown-linux-musl";

  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

function install() {
  const target = getTarget();
  // Release tarball is shell-acp-<version>-<target>.tar.gz and contains a
  // top-level directory <name>/ with the binary inside.
  const name = `${BIN_NAME}-${VERSION}-${target}`;
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${name}.tar.gz`;
  const binDir = path.join(__dirname, "bin");

  fs.mkdirSync(binDir, { recursive: true });

  const tmp = path.join(os.tmpdir(), `${BIN_NAME}-${VERSION}-${Date.now()}.tar.gz`);

  try {
    execSync(`curl -fsSL "${url}" -o "${tmp}"`, { stdio: "pipe" });
    // Strip the leading <name>/ directory, extracting just the binary.
    execSync(
      `tar xzf "${tmp}" -C "${binDir}" --strip-components=1 "${name}/${BIN_NAME}"`,
      { stdio: "pipe" }
    );
    fs.chmodSync(path.join(binDir, BIN_NAME), 0o755);
  } finally {
    try {
      fs.unlinkSync(tmp);
    } catch {}
  }

  console.log(`${BIN_NAME} ${VERSION} installed successfully`);
}

install();

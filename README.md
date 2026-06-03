# shell-acp

[![CI](https://github.com/meloalright/shell-acp/actions/workflows/ci.yml/badge.svg)](https://github.com/meloalright/shell-acp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Homebrew](https://img.shields.io/badge/homebrew--tap-v0.0.11-orange)](https://github.com/meloalright/homebrew-tap)

A shell exposed as an [Agent Client Protocol](https://agentclientprotocol.com) (ACP) agent.

shell-acp lets you drive a real terminal from a chat app. It speaks ACP (JSON-RPC 2.0 over stdio), so an ACP client such as [cc-connect](https://github.com/chenhg5/cc-connect) spawns it as a backend and bridges it to Telegram, Feishu, Slack, Discord, and more — every message becomes a command, and the output streams back.

### Why a real terminal emulator

Output is rendered through a headless [vt100](https://crates.io/crates/vt100) terminal, not by stripping escape codes. That means it handles what naive stripping can't:

- **Colors** are interpreted and flattened to clean text
- **Progress bars** (`\r` overwrites) collapse to their final line
- **REPLs** (`python3`, `node`, `mysql`) work — their prompts are learned for instant turn detection
- **Full-screen TUIs** (vim, `opencode`'s TUI, …) are detected and declined gracefully instead of spewing redraw noise

## Installation

### Homebrew

```sh
brew install meloalright/tap/shell-acp
```

### From source

```sh
cargo install --git https://github.com/meloalright/shell-acp.git
```

## Usage

shell-acp is an ACP agent — it isn't run directly, but spawned by an ACP client. Add it to your cc-connect `config.toml` as an `acp` agent:

```toml
[[projects]]
name = "shell"
admin_from = "<your-IM-user-id>"

[projects.agent]
type = "acp"

[projects.agent.options]
work_dir = "/root"
command = "/usr/local/bin/shell-acp"   # absolute path
display_name = "shell-acp terminal"

[[projects.platforms]]
type = "telegram"

[projects.platforms.options]
token = "<bot-token>"
allow_from = "<your-IM-user-id>"
```

Restart cc-connect, then talk to the bot. Send any shell command as a message; control the session in-band:

| Message | Effect |
|---|---|
| `<any command>` | run it; output streams back as one code block |
| `@shell ctrl-c` | interrupt the foreground job (Ctrl-C) |
| `@shell ctrl-d` | send EOF (exit a REPL/shell) |
| `@shell stop` | kill the shell session |

### CLI

```sh
shell-acp --help            # usage
shell-acp --version         # version
shell-acp --config <path>   # optional TOML config (targets + session tuning)
```

Without a config, sensible defaults apply (see [`config.example.toml`](config.example.toml)).

## Project Structure

```
shell-acp/
├── src/main.rs      # ACP stdio loop (initialize / session.* dispatch)
├── src/acp.rs       # JSON-RPC 2.0 request/response handling
├── src/router.rs    # session routing, output streaming, turn settling
├── src/session.rs   # PTY lifecycle + shell-integration startup
├── src/term.rs      # headless vt100 terminal rendering
├── src/buffer.rs    # prompt detection, sentinel/echo handling
├── src/config.rs    # config schema and defaults
└── src/target.rs    # cwd → shell resolution
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full design.

## License

MIT

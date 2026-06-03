# shell-acp

[![CI](https://github.com/meloalright/shell-acp/actions/workflows/ci.yml/badge.svg)](https://github.com/meloalright/shell-acp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://opensource.org/licenses/MIT)
[![Homebrew](https://img.shields.io/badge/dynamic/regex?url=https%3A%2F%2Fraw.githubusercontent.com%2Fmeloalright%2Fhomebrew-tap%2Frefs%2Fheads%2Fmain%2FFormula%2Fshell-acp.rb&search=version%20%22(%3F%3Cversion%3E%5Cd%2B(%3F%3A%5C.%5Cd%2B)%2B)%22&replace=%24%3Cversion%3E&label=homebrew-tap&logo=homebrew&color=f9d094)](https://github.com/meloalright/homebrew-tap/tree/main/Formula/shell-acp.rb)
[![npm](https://img.shields.io/npm/v/shell-acp.svg?logo=npm&color=red)](https://www.npmjs.com/package/shell-acp)

A shell exposed as an ACP agent.

It speaks ACP (JSON-RPC 2.0 over stdio), so an ACP client such as [cc-connect](https://github.com/chenhg5/cc-connect) spawns it as a backend and bridges it to Telegram, Lark, Slack, Discord, and more — every message becomes a command, and the output streams back.

> 一款命令行的对外 ACP 实现。可用 ACP 客户端如 [cc-connect](https://github.com/chenhg5/cc-connect) 把它链接至飞书、微信、QQ 等平台 —— 以实现在聊天中运行命令行。

<p align="center">
  <img src="https://github.com/user-attachments/assets/f265479c-ff8e-487d-ac94-a8a30d93d2bf" alt="Telegram" width="32%" />
  <img src="https://github.com/user-attachments/assets/aec62230-b0a6-4fe8-ba31-044385f0fc5b" alt="飞书" width="32%" />
</p>
<p align="center">
  <em>Left: Telegram &nbsp;|&nbsp; Right: 飞书</em>
</p>


## Installation

##### ⚡️ install via homebrew

```sh
brew install meloalright/tap/shell-acp
```

##### ⚡️ install via npm

```sh
npm install -g shell-acp
```

## Usage

Add it to your cc-connect `config.toml` as an `acp` agent:

```toml
[projects.agent]
type = "acp"

[projects.agent.options]
work_dir = "/root"
command = "/usr/local/bin/shell-acp"
display_name = "shell-acp terminal"
```

Restart cc-connect, then talk to the bot. Send any shell command as a message; control the session in-band:

| Message | Effect |
|---|---|
| `<any command>` | run it; output streams back as one code block |
| `@shell ctrl-c` | interrupt the foreground job (Ctrl-C) |
| `@shell ctrl-d` | send EOF (exit a REPL/shell) |
| `@shell stop` | kill the shell session |


# IM Local Terminal

> 通过 IM 软件远程操控本机 Shell 的轻量守护进程

## 定位

不做 SSH，不做远程服务器。只做一件事：**把 IM 会话绑定到本机 PTY**。

## 架构

```
IM 软件 (飞书 / Telegram / ...)
  ↓
cc-connect / Bot
  ↓
im-terminal daemon
  ↓
local PTY (portable-pty)
  ↓
bash / zsh
```

每一层职责清晰：

| 层 | 职责 |
|---|---|
| IM 软件 | 用户界面，消息收发 |
| cc-connect / Bot | IM 协议适配，消息路由 |
| im-terminal daemon | 会话管理，权限校验，PTY 生命周期 |
| local PTY | 进程隔离，终端模拟 |
| bash / zsh | 实际命令执行 |

## 从 cc-connect 只学 4 件事

1. **IM 消息怎么进来** — 消息入口协议与格式
2. **chat_id / user_id 怎么识别** — 会话与用户的唯一标识
3. **流式回复怎么发回去** — 输出缓冲与分片推送
4. **admin_from / 权限怎么限制** — 谁有资格操作 shell

其余一概不关心。

## 实现目标

**核心原则：每个 IM 会话绑定一个本机 shell。**

### MVP 命令

| 命令 | 作用 |
|---|---|
| `@term start` | 为当前会话启动一个 shell |
| `@term stop` | 销毁当前会话的 shell |
| `@term ctrl-c` | 向当前 PTY 发送 SIGINT |
| `@term sessions` | 列出所有活跃会话 |

### 普通消息 = 直接写入 PTY

```
用户：cd ~/project
bot ：~/project $

用户：cargo test
bot ：running tests...
     test result: ok. 42 passed; 0 failed
```

没有前缀，没有转义，打字即执行。

## 核心 Rust 结构

```rust
DashMap<ChatId, LocalTerminalSession>
```

```rust
struct LocalTerminalSession {
    chat_id: String,
    owner_user_id: String,
    cwd: PathBuf,
    child: Box<dyn Child + Send>,
    stdin: Box<dyn Write + Send>,
}
```

### 本地执行

使用 `portable-pty` crate 启动 PTY：

```
bash --noprofile --norc
```

或

```
zsh
```

## 权限模型

安全边界归结为一句话：**谁能控制这台机器上的 shell。**

### 配置

```toml
[admins]
users = ["melo"]

[targets.dev]
cwd = "/Users/melo/project"
shell = "zsh"
allowed_chats = ["feishu_group_xxx"]
```

### 校验逻辑

1. 消息进来，先查 `user_id` 是否在 `admins.users` 白名单
2. 再查 `chat_id` 是否在对应 target 的 `allowed_chats`
3. 两项都通过才放行

不在白名单 = 静默丢弃，不回复，不报错。

## V1 范围

只做 4 件事，多一件都不做：

1. **Admin 白名单** — 硬编码级别的访问控制
2. **本地 PTY** — portable-pty 管理 shell 生命周期
3. **会话隔离** — 每个 chat_id 独立 shell，互不干扰
4. **输出缓冲** — PTY 输出按时间窗口或字节数聚合后发回 IM，避免消息洪水

### 不做的事

- 不做 SSH / 远程连接
- 不做文件传输
- 不做多用户协作同一 shell
- 不做命令审计 / 回放（V2 再说）
- 不做 Web UI

## 技术选型

| 组件 | 选择 | 理由 |
|---|---|---|
| 语言 | Rust | 安全、性能、单二进制分发 |
| PTY | portable-pty | 跨平台 PTY 抽象 |
| 并发会话 | DashMap | 无锁并发 HashMap |
| 异步运行时 | tokio | 生态成熟 |
| 配置格式 | TOML | Rust 生态标配 |
| IM 对接 | cc-connect 协议 | 复用现有 Bot 基础设施 |

## 输出缓冲策略

PTY 输出是连续字节流，IM 消息是离散的。需要缓冲：

```
PTY stdout → buffer → 聚合窗口 (300ms or 4KB) → IM 消息
```

- **时间窗口**：最后一次输出后 300ms 无新输出，flush buffer
- **字节上限**：buffer 达到 4KB，立即 flush
- **消息长度限制**：单条 IM 消息不超过平台限制，超出则分片

## 交互示例

```
melo  : @term start
bot   : shell started (zsh) | session: feishu_group_xxx

melo  : pwd
bot   : /Users/melo/project

melo  : ls -la
bot   : total 128
        drwxr-xr-x  12 melo staff  384 Jun  1 10:00 .
        drwxr-xr-x   5 melo staff  160 May 28 09:00 ..
        -rw-r--r--   1 melo staff  842 Jun  1 09:55 Cargo.toml
        ...

melo  : cargo build 2>&1
bot   : Compiling terminal-connect v0.1.0
        Finished dev [unoptimized + debuginfo] target(s) in 12.34s

melo  : @term ctrl-c
bot   : SIGINT sent

melo  : @term stop
bot   : shell terminated | session: feishu_group_xxx

melo  : @term sessions
bot   : no active sessions
```

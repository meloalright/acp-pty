# terminal-connect 架构文档

> 基于 cc-connect 源码研究（Go, `/root/cc-connect`）推导出的 Rust 实现架构

## 1. 与 cc-connect 的关系

terminal-connect **不重新实现 IM 协议适配**。cc-connect 已经完成了所有脏活：

```
飞书 / Telegram / Slack / Discord / 企业微信 / LINE / DingTalk / QQ
                        ↓
              cc-connect (Go, 已有)
          ┌─────────────┼─────────────┐
          ↓             ↓             ↓
     Claude Code   terminal-connect  其他 backend
     (现有后端)      (本项目)
```

cc-connect 通过 **stdio 协议** 与后端通信——它启动一个子进程，用 stdin/stdout 交换 JSON 消息。terminal-connect 要做的就是实现这个 stdio 协议，替代 Claude Code 成为后端。

### cc-connect 已经帮我们做了的事

| 能力 | cc-connect 如何实现 | terminal-connect 是否需要关心 |
|---|---|---|
| IM 协议适配 | 每个平台一个 adapter（WebSocket/Webhook/Polling） | 不需要 |
| 消息归一化 | 所有平台 → `core.Message` 统一结构 | 不需要，只收 stdin JSON |
| SessionKey 路由 | `{platform}:{chatID}:{userID}` 格式 | 直接使用，作为会话映射的 key |
| 流式消息编辑 | `PreviewStarter` / `UpdateMessage` 接口 | 不需要，cc-connect 处理 |
| 消息分片 | 4000 rune 上限自动切割 | 不需要，cc-connect 处理 |
| 权限第一层 | `allow_from` 在平台层静默丢弃 | 不需要 |

### terminal-connect 只需要实现

| 能力 | 说明 |
|---|---|
| stdio JSON 协议 | 从 stdin 读消息，向 stdout 写回复 |
| SessionKey → PTY 映射 | 用 SessionKey 做会话隔离 |
| PTY 生命周期管理 | 创建、写入、读取、销毁 |
| 输出缓冲 | PTY 字节流 → 离散文本块 |
| 权限第二层 | admin 白名单 + allowed_chats |

## 2. 消息入口：从 cc-connect 学到的

### cc-connect 的消息流（已有，不改）

```
IM 平台事件
  ↓
Platform adapter（feishu/telegram/slack/...）
  ↓  各自协议：WebSocket / HTTP Webhook / Long Polling
  ↓  各自提取：chatID, userID, content, attachments
  ↓
归一化为 core.Message {
    SessionKey:  "feishu:oc_abc123:ou_xyz789"
    Platform:    "feishu"
    UserID:      "ou_xyz789"
    UserName:    "melo"
    ChatName:    "dev-group"
    Content:     "cd ~/project"
    MessageID:   "msg_xxx"
    ReplyCtx:    <platform-specific>
}
  ↓
engine.handleMessage()  →  异步 dispatch (go p.dispatchMessage)
  ↓
路由到后端进程 stdin
```

### terminal-connect 的消息入口（本项目实现）

```
cc-connect stdout → terminal-connect stdin
  ↓
逐行读取 JSON
  ↓
解析为 IncomingMessage {
    session_key: String,     // "feishu:oc_abc123:ou_xyz789"
    user_id:     String,     // "ou_xyz789"
    user_name:   String,     // "melo"
    chat_name:   String,     // "dev-group"
    content:     String,     // "cd ~/project"
    platform:    String,     // "feishu"
}
  ↓
路由到 SessionRouter
```

**关键决策**：terminal-connect 是 cc-connect 的子进程，通过 stdin/stdout 通信。不需要监听端口，不需要 HTTP server，不需要 WebSocket。

## 3. 会话标识：从 cc-connect 学到的

### cc-connect 的 SessionKey 设计

cc-connect 用 `SessionKey` 作为全局唯一的会话路由键：

| 模式 | SessionKey 格式 | 场景 |
|---|---|---|
| 默认（per-user） | `{platform}:{chatID}:{userID}` | 同群不同用户各自独立 |
| 共享频道 | `{platform}:{chatID}` | 整个群共享一个会话 |
| 线程隔离 | `{platform}:{chatID}:root:{messageID}` | 飞书话题模式 |

**代码位置**：
- Telegram: `fmt.Sprintf("telegram:%d:%d", chatID, userID)` — `platform/telegram/telegram.go:338`
- Feishu: `fmt.Sprintf("feishu:%s:%s", chatID, userID)` — `platform/feishu/feishu.go:2338`
- 引擎路由: `e.interactiveStates[msg.SessionKey]` — `core/engine.go:862`

### terminal-connect 的会话映射

直接复用 SessionKey，不做二次加工：

```rust
type SessionKey = String;  // 直接用 cc-connect 传来的值

struct SessionRouter {
    sessions: DashMap<SessionKey, LocalTerminalSession>,
}
```

**映射规则**：
- 一个 SessionKey → 一个 PTY → 一个 shell 进程
- SessionKey 由 cc-connect 生成并传入，terminal-connect 视为不透明字符串
- `@term start` 创建映射，`@term stop` 销毁映射
- 普通消息查找映射，不存在则提示用户先 `@term start`

## 4. 输出回传：从 cc-connect 学到的

### cc-connect 的流式回复三层架构

```
Agent 输出事件
  ↓
StreamPreview 层
  ├── 节流：间隔 ≥ 1500ms 才更新
  ├── 最小增量：≥ 30 字符才 flush
  └── 预览上限：中间态最多 2000 字符
  ↓
消息分片层
  ├── 上限：4000 rune / 条
  └── 优先在换行符处切割（≥ 50% 块大小时）
  ↓
平台发送层
  ├── SendPreviewStart() → 初始消息
  ├── UpdateMessage()    → 原地编辑（流式效果）
  └── DeletePreviewMessage() → 降级时删除预览
```

**降级策略**：`UpdateMessage()` 失败 → 标记 `degraded = true` → 回退到独立 `Send()`

**代码位置**：
- 节流逻辑: `core/streaming.go:142-160`
- 分片: `core/engine.go:10906-10936`, `splitMessage()` 在 4000 rune 处切
- 降级: `core/streaming.go:192-214`

### terminal-connect 的输出缓冲（本项目实现）

PTY 输出与 AI 输出有本质区别：PTY 是**连续字节流**（每个字符都可能触发一次 read），AI 是**离散事件流**（EventText 粒度已经是词/句级别）。所以缓冲策略需要不同：

```
PTY stdout (连续字节流)
  ↓
OutputBuffer {
    buffer: Vec<u8>,
    last_activity: Instant,
    flush_timer: tokio::time::Sleep,
}
  ↓ 触发条件（任一）
  ├── 静默超时：最后一次 read 后 300ms 无新数据
  ├── 字节上限：buffer ≥ 4096 bytes
  └── 显式 flush：收到 @term ctrl-c 等命令时
  ↓
strip_ansi_escapes()  // 清除颜色/光标控制码
  ↓
truncate_if_needed()  // 超长输出截断，保留头尾
  ↓
写入 stdout JSON → cc-connect → IM
```

**与 cc-connect 的分工**：
- terminal-connect 负责：字节流聚合、ANSI 清除、语义截断
- cc-connect 负责：消息分片（4000 rune）、流式预览编辑、平台 API 调用

**为什么不在 terminal-connect 做消息分片**：cc-connect 已经做了，而且它知道各平台的具体限制。terminal-connect 只需要输出合理大小的文本块（≤ 8KB），cc-connect 会处理剩下的。

## 5. 权限模型：从 cc-connect 学到的

### cc-connect 的两层权限

```
消息进入
  ↓
第一层：Platform allow_from（平台级）
  ├── 检查 user_id 是否在 allow_from 白名单
  ├── 支持 "*" 通配符
  ├── 不通过 → 静默丢弃，不回复
  └── 代码：core.AllowList() — core/message.go:54-65
  ↓
第二层：Command admin_from（命令级）
  ├── 特权命令（/shell, /restart 等）需要 admin 身份
  ├── isAdmin() 检查 — core/engine.go:790-806
  ├── 不通过 → 回复 "Admin privilege required"
  └── 逗号分隔列表，大小写不敏感匹配
```

**要点**：
- `allow_from` 是**平台全局**的，不区分群组
- `admin_from` 是**项目全局**的，不区分群组
- 两层独立，第一层过滤噪音，第二层保护敏感操作

### terminal-connect 的权限模型（本项目实现）

terminal-connect 的安全边界比 cc-connect 更严格——因为这里控制的是**真实 shell**，不是 AI 对话。

```
消息从 cc-connect stdin 进入
  ↓
第一层：admin 白名单（必须通过）
  ├── 检查 user_id ∈ config.admins.users
  ├── 不通过 → 静默丢弃（不告诉攻击者此服务存在）
  └── 无 "*" 通配符（shell 访问不能开放给所有人）
  ↓
第二层：chat 白名单（必须通过）
  ├── 检查 session_key 中的 chat_id 部分
  ├── 匹配 config.targets.*.allowed_chats
  ├── 不通过 → 静默丢弃
  └── 同时决定该会话绑定到哪个 target（cwd, shell）
  ↓
放行 → 路由到 SessionRouter
```

```toml
# config.toml

[admins]
users = ["ou_melo_openid", "telegram_12345"]   # 跨平台 user_id

[targets.dev]
cwd = "/Users/melo/project"
shell = "zsh"
allowed_chats = ["feishu:oc_group1", "telegram:-100123"]

[targets.ops]
cwd = "/opt/services"
shell = "bash"
allowed_chats = ["feishu:oc_ops_group"]
```

**与 cc-connect 的关键区别**：

| 维度 | cc-connect | terminal-connect |
|---|---|---|
| 第一层粒度 | 平台级（allow_from 不区分群） | 保留，但依赖 cc-connect 执行 |
| 第二层粒度 | 项目级（admin_from 不区分群） | **chat 级**（allowed_chats 按群控制） |
| 通配符 | 支持 `"*"` | **禁止**（shell 不能开放） |
| 失败响应 | allow_from 静默 / admin_from 报错 | **全部静默**（不暴露服务存在） |
| 绑定关系 | 无 | chat → target（决定 cwd 和 shell） |

## 6. 核心模块设计

```
terminal-connect/
├── src/
│   ├── main.rs              # 入口：stdin 读循环 + 信号处理
│   ├── protocol.rs          # cc-connect stdio JSON 协议
│   │   ├── IncomingMessage   # stdin 解析
│   │   └── OutgoingMessage   # stdout 序列化
│   ├── router.rs            # 消息路由
│   │   ├── SessionRouter     # DashMap<SessionKey, Session>
│   │   └── CommandParser     # @term start/stop/ctrl-c/sessions
│   ├── session.rs           # PTY 会话管理
│   │   ├── LocalTerminalSession
│   │   ├── spawn_pty()       # portable-pty 启动 shell
│   │   └── write_to_pty()    # 用户输入写入 stdin
│   ├── buffer.rs            # 输出缓冲
│   │   ├── OutputBuffer      # 字节聚合 + 定时 flush
│   │   └── strip_ansi()      # ANSI 转义清除
│   ├── auth.rs              # 权限校验
│   │   ├── AdminCheck        # user_id 白名单
│   │   └── ChatCheck         # chat_id → target 映射
│   └── config.rs            # TOML 配置加载
│       ├── Config
│       ├── AdminConfig
│       └── TargetConfig
├── config.example.toml
├── Cargo.toml
└── CONCEPT.md
```

## 7. 数据流详解

### 7.1 完整请求生命周期

```
用户在飞书群输入 "ls -la"
  ↓
飞书服务器 → cc-connect (WebSocket)
  ↓ 平台 adapter 提取：
  ↓   chatID = "oc_abc123"
  ↓   userID = "ou_melo"
  ↓   content = "ls -la"
  ↓   sessionKey = "feishu:oc_abc123:ou_melo"
  ↓
cc-connect engine.handleMessage()
  ↓ allow_from 检查 ✓
  ↓ 写入后端 stdin:
  ↓   {"session_key":"feishu:oc_abc123:ou_melo","user_id":"ou_melo","content":"ls -la",...}
  ↓
terminal-connect stdin 读取
  ↓ protocol.rs: 解析 IncomingMessage
  ↓ auth.rs: admin 白名单 ✓, chat 白名单 ✓ → target=dev
  ↓ router.rs: 查找 sessions["feishu:oc_abc123:ou_melo"]
  ↓ session.rs: write_to_pty("ls -la\n")
  ↓
PTY 执行 ls -la
  ↓ stdout 输出字节流
  ↓
buffer.rs: OutputBuffer 聚合
  ↓ 300ms 无新输出 → flush
  ↓ strip_ansi() 清除控制码
  ↓
terminal-connect stdout 写出:
  {"session_key":"feishu:oc_abc123:ou_melo","content":"total 128\ndrwxr-xr-x ..."}
  ↓
cc-connect 读取后端 stdout
  ↓ StreamPreview / UpdateMessage 实时编辑消息
  ↓ 超长则 splitMessage() 分片
  ↓
飞书 API → 用户看到结果
```

### 7.2 @term 命令处理流

```
"@term start"
  ↓
CommandParser::parse("@term start") → Command::Start
  ↓
SessionRouter::handle_start(session_key, target)
  ↓
session.rs::spawn_pty(SpawnConfig {
    shell: target.shell,       // "zsh"
    cwd: target.cwd,           // "/Users/melo/project"
    env: filtered_env(),       // 最小化环境变量
})
  ↓
portable_pty::CommandBuilder::new(shell)
    .cwd(cwd)
    .env(...)
  ↓
sessions.insert(session_key, LocalTerminalSession {
    session_key,
    owner_user_id: user_id,
    target_name: "dev",
    pty_pair: pair,            // master + slave
    child: child_process,
    reader: BufReader(master.try_clone_reader()),
    writer: master.take_writer(),
    buffer: OutputBuffer::new(),
    created_at: Instant::now(),
})
  ↓
tokio::spawn(output_read_loop(session_key))  // 后台读 PTY 输出
  ↓
回复: "shell started (zsh) | session: feishu:oc_abc123:ou_melo | cwd: /Users/melo/project"
```

### 7.3 输出读取后台循环

```rust
// 每个 session 一个 tokio task
async fn output_read_loop(key: SessionKey, reader: PtyReader, buffer: OutputBuffer) {
    let mut buf = [0u8; 1024];
    loop {
        select! {
            n = reader.read(&mut buf) => {
                match n {
                    Ok(0) => break,  // PTY 关闭（shell 退出）
                    Ok(n) => buffer.append(&buf[..n]),
                    Err(_) => break,
                }
            }
            chunk = buffer.next_flush() => {
                // 300ms 静默超时 或 4KB 上限触发
                let text = strip_ansi(&chunk);
                let text = truncate_if_needed(&text, 8192);
                send_stdout(OutgoingMessage {
                    session_key: key.clone(),
                    content: text,
                });
            }
        }
    }
    // shell 退出，清理 session
    sessions.remove(&key);
    send_stdout(OutgoingMessage {
        session_key: key,
        content: "shell exited".into(),
    });
}
```

## 8. 协议定义

### 8.1 stdin（cc-connect → terminal-connect）

```json
{
    "session_key": "feishu:oc_abc123:ou_melo",
    "platform": "feishu",
    "user_id": "ou_melo",
    "user_name": "melo",
    "chat_name": "dev-group",
    "content": "ls -la",
    "message_id": "msg_xxx"
}
```

每条消息一行 JSON（newline-delimited JSON, NDJSON）。

### 8.2 stdout（terminal-connect → cc-connect）

```json
{
    "session_key": "feishu:oc_abc123:ou_melo",
    "type": "text",
    "content": "total 128\ndrwxr-xr-x  12 melo staff  384 Jun  1 10:00 .\n..."
}
```

| type | 含义 |
|---|---|
| `text` | PTY 输出文本 |
| `system` | 系统消息（session started/stopped/exited） |
| `error` | 错误信息 |

### 8.3 stderr（日志）

terminal-connect 的日志写 stderr，不干扰 stdin/stdout 协议通道。cc-connect 可选择捕获或丢弃。

## 9. 安全设计

### 9.1 攻击面分析

| 威胁 | 缓解 |
|---|---|
| 未授权用户执行命令 | 双层白名单（admin + chat），全部静默丢弃 |
| PTY 逃逸 | portable-pty 在独立进程中运行，不共享 terminal-connect 的 fd |
| 环境变量泄露 | spawn_pty 使用最小化 env，不继承 terminal-connect 的全部环境 |
| 输出注入 IM | strip_ansi 清除控制码，防止在 IM 端渲染异常内容 |
| 资源耗尽 | 最大并发 session 数限制（默认 16），单 session 输出速率限制 |
| Shell 进程僵尸 | child process reaper：定期检查 + session 空闲超时自动 kill |

### 9.2 最小化环境变量

```rust
fn filtered_env() -> Vec<(String, String)> {
    vec![
        ("PATH", "/usr/local/bin:/usr/bin:/bin"),
        ("HOME", &target.cwd),
        ("TERM", "xterm-256color"),
        ("LANG", "en_US.UTF-8"),
        // 不继承：AWS_*, GITHUB_TOKEN, SSH_*, 等
    ]
}
```

### 9.3 空闲超时

```toml
[session]
idle_timeout_secs = 1800    # 30 分钟无输入自动销毁
max_sessions = 16           # 全局最大并发会话
max_output_buffer = 65536   # 单次输出缓冲上限 64KB
```

## 10. 从 cc-connect 借鉴 vs 自己实现 — 总结

| 能力 | cc-connect 负责 | terminal-connect 负责 |
|---|---|---|
| IM 协议适配 | ✅ 7+ 平台 adapter | - |
| SessionKey 生成 | ✅ `{platform}:{chatID}:{userID}` | 直接使用 |
| 消息归一化 | ✅ `core.Message` | 解析 stdin JSON |
| allow_from 过滤 | ✅ 平台层静默丢弃 | - |
| admin 白名单 | - | ✅ user_id 检查 |
| chat 白名单 | - | ✅ chat_id → target 映射 |
| 流式消息编辑 | ✅ PreviewStarter/UpdateMessage | - |
| 消息分片 | ✅ 4000 rune 切割 | - |
| 输出缓冲 | - | ✅ 300ms/4KB 聚合 |
| ANSI 清除 | - | ✅ strip_ansi_escapes |
| PTY 管理 | - | ✅ portable-pty |
| 会话生命周期 | - | ✅ start/stop/timeout/reap |

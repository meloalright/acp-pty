# acp-pty 架构文档

> ACP 协议到本地 PTY 的翻译层 — cc-connect 生态的 shell 后端
>
> 基于 cc-connect 源码研究（Go, `/root/cc-connect`）推导

## 1. 与 cc-connect 的关系

acp-pty **不重新实现 IM 协议适配**。cc-connect 已经完成了所有脏活：

```
飞书 / Telegram / Slack / Discord / 企业微信 / LINE / DingTalk / QQ
                        ↓
              cc-connect (Go, 已有)
          ┌─────────────┼─────────────┐
          ↓             ↓             ↓
     Claude Code   acp-pty  其他 backend
     (现有后端)      (本项目)
```

cc-connect 通过 **ACP（Agent Client Protocol）** 与后端通信——它启动一个子进程，用 stdin/stdout 交换 **JSON-RPC 2.0** 消息。acp-pty 要做的就是实现 ACP 协议的最小子集，作为 `type = "acp"` agent 接入 cc-connect。

### cc-connect 已经帮我们做了的事

| 能力 | cc-connect 如何实现 | acp-pty 是否需要关心 |
|---|---|---|
| IM 协议适配 | 每个平台一个 adapter（WebSocket/Webhook/Polling） | 不需要 |
| 消息归一化 | 所有平台 → `core.Message` 统一结构 | 不需要，只收 ACP JSON-RPC |
| SessionKey 路由 | `{platform}:{chatID}:{userID}` 格式 | 不直接可见，cc-connect 内部路由；acp-pty 用 ACP sessionId |
| 流式消息编辑 | `PreviewStarter` / `UpdateMessage` 接口 | 不需要，cc-connect 处理 |
| 消息分片 | 4000 rune 上限自动切割 | 不需要，cc-connect 处理 |
| 权限第一层 | `allow_from` 在平台层静默丢弃 | 不需要 |

### acp-pty 只需要实现

| 能力 | 说明 |
|---|---|
| ACP 协议子集 | JSON-RPC 2.0 over stdio：`initialize`、`session/new`、`session/prompt` |
| sessionId → PTY 映射 | 用 ACP sessionId 做会话隔离 |
| PTY 生命周期管理 | 创建、写入、读取、销毁 |
| 输出缓冲 | PTY 字节流 → `session/update` 通知 |
| cwd → target 绑定 | cwd 决定 shell 类型（admin 白名单由 cc-connect admin_from 处理） |

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

### acp-pty 的消息入口（本项目实现）

acp-pty 是 cc-connect 的子进程，通过 ACP（JSON-RPC 2.0 over stdio）通信。

```
cc-connect → acp-pty stdin: JSON-RPC request
  ↓
逐行读取，按 JSON-RPC 2.0 分发：
  ├── "initialize"       → 握手，返回能力声明
  ├── "session/new"      → 创建 PTY 会话
  ├── "session/load"     → 恢复 PTY 会话（V1 直接失败，触发 session/new）
  ├── "session/prompt"   → 用户输入写入 PTY
  └── "session/list"     → 列出活跃会话
  ↓
acp-pty → cc-connect stdout: JSON-RPC response + notifications
```

**关键决策**：不需要监听端口，不需要 HTTP server，不需要 WebSocket。ACP 协议本身就是 stdio。

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

### acp-pty 的会话映射

cc-connect 的 SessionKey（`{platform}:{chatID}:{userID}`）在 ACP 层不直接暴露给后端。acp-pty 通过 ACP 的 **sessionId** 管理会话：

```rust
type SessionId = String;  // acp-pty 自己生成，返回给 cc-connect

struct SessionRouter {
    sessions: DashMap<SessionId, LocalTerminalSession>,
}
```

**两层 session 的关系**：

```
cc-connect 侧                          acp-pty 侧
─────────────                           ──────────────────
SessionKey                              
"feishu:oc_abc:ou_melo"                 
  ↓ engine 路由                         
ACP session/new(cwd)    ──stdin──→      生成 sessionId = "term-{uuid}"
                        ←─stdout──      返回 {sessionId: "term-{uuid}"}
  ↓ 缓存 sessionId                     sessions["term-{uuid}"] = PTY
ACP session/prompt(     ──stdin──→      查找 sessions["term-{uuid}"]
  sessionId, prompt)                    写入 PTY stdin
                        ←─stdout──      session/update 通知（PTY 输出）
```

**生命周期规则**：
- `session/new` → **立即创建 PTY 并 spawn shell**，返回 sessionId。用户第一条消息直接写入 PTY，无需 `@term start`
- `session/prompt` → 查找 sessionId 对应的 PTY，写入用户输入
- `session/load` → V1 始终返回错误（PTY 不可跨进程恢复），cc-connect 会 fallback 到 `session/new`
- 用户发 `@term stop` → 作为普通 prompt 进入，acp-pty 内部解析后销毁 PTY 并清理 session
- cc-connect 重启 → 子进程被杀，所有 PTY 随之销毁；cc-connect 重新 spawn acp-pty 并调 `session/new`

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

### acp-pty 的输出缓冲（本项目实现）

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
session/update notification → cc-connect → IM
```

**与 cc-connect 的分工**：
- acp-pty 负责：字节流聚合、ANSI 清除、语义截断、判断"本轮输出结束"
- cc-connect 负责：消息分片（4000 rune）、流式预览编辑、平台 API 调用

**为什么不在 acp-pty 做消息分片**：cc-connect 已经做了，而且它知道各平台的具体限制。acp-pty 只需要输出合理大小的文本块（≤ 8KB），cc-connect 会处理剩下的。

### session/prompt 的 RPC 返回时机

cc-connect 把 `session/prompt` RPC 返回当作**本轮结束信号**（触发 `EventResult(Done=true)`，finalize IM 消息）。所以 acp-pty 不能立即返回，必须等 PTY 输出稳定后再返回。

```
write_to_pty("ls -la\n")
  ↓
OutputBuffer 持续聚合 PTY 输出
  ↓ 每次 flush → 发送 session/update notification（流式推送）
  ↓
最后一次 flush 后，再等一个 300ms 静默窗口
  ↓ 确认无新输出
  ↓
返回 session/prompt RPC response: {"jsonrpc":"2.0","id":3,"result":{}}
  ↓
cc-connect: EventResult(Done=true) → finalize IM 消息
```

**安全阀：30 秒硬上限**。长命令（`cargo build`、`make`）可能持续输出数分钟。超过 30 秒后强制返回 RPC，后续输出继续通过 notification best-effort 推送（cc-connect 可能显示为新消息）。

| 场景 | 行为 |
|---|---|
| 快命令（ls, pwd, cat） | 输出秒完，300ms 静默后返回 |
| 中等命令（cargo test） | 输出几秒内结束，300ms 静默后返回 |
| 长命令（cargo build） | 前 30s 输出 + notification 流式推送，30s 时强制返回 RPC，后续 best-effort |
| 无输出命令（cd, export） | 写入 PTY 后 300ms 无输出，直接返回 |

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

### acp-pty 的权限模型（本项目实现）

#### 权限分工：cc-connect 管人，acp-pty 管路由

cc-connect 已经有两道权限门：
1. `allow_from` — 平台层，过滤非白名单用户（静默丢弃）
2. `admin_from` — 项目层，过滤非管理员（回复错误）

能到达 acp-pty stdin 的消息，**用户身份已经被 cc-connect 校验过了**。acp-pty 不需要重复做 user 白名单。

acp-pty 只需要做一件事：**chat → target 绑定**（决定这个会话用哪个 cwd 和 shell）。

```
消息经过 cc-connect allow_from + admin_from 校验后
  ↓
session/new(cwd) 到达 acp-pty
  ↓
chat → target 绑定
  ├── session/new 的 cwd 参数来自 cc-connect 配置
  ├── acp-pty 校验 cwd 是否在允许列表中
  ├── 匹配 config.targets.*.cwd → 决定 shell 类型
  ├── 不匹配 → 返回 JSON-RPC error
  └── 匹配 → 创建 PTY
  ↓
放行 → 创建 PTY 会话
```

```toml
# acp-pty config.toml

[targets.dev]
cwd = "/Users/melo/project"
shell = "zsh"

[targets.ops]
cwd = "/opt/services"
shell = "bash"
```

#### cc-connect 侧配置

```toml
# cc-connect config.toml

[[projects]]
name = "terminal"
admin_from = "ou_melo_openid,telegram_12345"  # 谁能用

[projects.agent]
type = "acp"

[projects.agent.options]
command = "/usr/local/bin/acp-pty"
args = ["--config", "/etc/acp-pty.toml"]
work_dir = "/Users/melo/project"  # 传给 session/new 的 cwd
```

#### 权限分层总结

| 层 | 谁负责 | 做什么 | 失败行为 |
|---|---|---|---|
| allow_from | cc-connect | 过滤非白名单用户 | 静默丢弃 |
| admin_from | cc-connect | 过滤非管理员 | 回复 "Admin privilege required" |
| target 绑定 | acp-pty | cwd → shell 映射 | JSON-RPC error |

**设计理由**：不在 acp-pty 重复做 user 白名单，避免 4 层权限检查的维护负担。cc-connect 的 `admin_from` 已经足够严格——配置时不用 `"*"` 通配符即可。安全边界在 cc-connect 层就闭合了。

## 6. 核心模块设计

```
acp-pty/
├── src/
│   ├── main.rs              # 入口：stdin JSON-RPC 读循环 + 信号处理
│   ├── acp.rs               # ACP 协议层（JSON-RPC 2.0）
│   │   ├── RpcMessage        # 请求/响应/通知的统一类型
│   │   ├── handle_initialize # 握手 + 能力声明
│   │   ├── handle_session_new    # 创建 PTY
│   │   ├── handle_session_prompt # 用户输入写 PTY
│   │   ├── handle_session_load   # V1: 始终返回错误
│   │   └── send_notification     # session/update 通知
│   ├── router.rs            # 会话路由
│   │   ├── SessionRouter     # DashMap<SessionId, Session>
│   │   └── CommandParser     # @term stop/ctrl-c
│   ├── session.rs           # PTY 会话管理
│   │   ├── LocalTerminalSession
│   │   ├── spawn_pty()       # portable-pty 启动 shell
│   │   └── write_to_pty()    # 用户输入写入 stdin
│   ├── buffer.rs            # 输出缓冲
│   │   ├── OutputBuffer      # 字节聚合 + 定时 flush
│   │   └── strip_ansi()      # ANSI 转义清除
│   ├── target.rs            # target 绑定
│   │   └── resolve_target()  # cwd → TargetConfig 映射
│   └── config.rs            # TOML 配置加载
│       ├── Config
│       └── TargetConfig
├── config.example.toml
├── Cargo.toml
└── CONCEPT.md
```

## 7. 数据流详解

### 7.1 首次连接：initialize + session/new

```
cc-connect spawn acp-pty 子进程
  ↓
cc-connect stdin →
  {"jsonrpc":"2.0","id":1,"method":"initialize",
   "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{}}}

acp-pty stdout →
  {"jsonrpc":"2.0","id":1,"result":{
    "protocolVersion":1,
    "agentCapabilities":{"loadSession":false,"sessionCapabilities":{}}
  }}
  ↓
cc-connect stdin →
  {"jsonrpc":"2.0","id":2,"method":"session/new",
   "params":{"cwd":"/Users/melo/project","mcpServers":[]}}

acp-pty 内部：
  ↓ target.rs: resolve_target("/Users/melo/project") → target=dev, shell=zsh
  ↓ session.rs: spawn_pty(zsh, cwd="/Users/melo/project")
  ↓ sessions.insert("term-a1b2c3", LocalTerminalSession {...})
  ↓ tokio::spawn(output_read_loop("term-a1b2c3"))

acp-pty stdout →
  {"jsonrpc":"2.0","id":2,"result":{"sessionId":"term-a1b2c3"}}
```

### 7.2 用户输入：session/prompt

```
用户在飞书群输入 "ls -la"
  ↓
飞书服务器 → cc-connect (WebSocket)
  ↓ 平台 adapter: allow_from ✓, admin_from ✓
  ↓
cc-connect stdin →
  {"jsonrpc":"2.0","id":3,"method":"session/prompt",
   "params":{"sessionId":"term-a1b2c3",
             "prompt":[{"type":"text","text":"ls -la"}]}}
  ↓
acp-pty 内部：
  ↓ acp.rs: 解析 session/prompt
  ↓ router.rs: 查找 sessions["term-a1b2c3"]
  ↓ CommandParser: "ls -la" 不是 @term 命令 → 普通输入
  ↓ session.rs: write_to_pty("ls -la\n")
  ↓
PTY 执行 ls -la → stdout 输出字节流
  ↓
buffer.rs: OutputBuffer 聚合（300ms 静默 / 4KB 上限）
  ↓ strip_ansi() 清除控制码
  ↓
acp-pty stdout → session/update 通知（可能多条，流式推送）：
  {"jsonrpc":"2.0","method":"session/update",
   "params":{"sessionId":"term-a1b2c3","update":{
     "sessionUpdate":"agent_message_chunk",
     "content":{"type":"text","text":"total 128\ndrwxr-xr-x  12 melo ..."}
   }}}
  ↓
cc-connect 实时处理 notification
  ↓ StreamPreview / UpdateMessage 实时编辑 IM 消息
  ↓ 超长则 splitMessage() 分片
  ↓
最后一次 buffer flush 后 300ms 无新输出（或 30s 硬上限）
  ↓
acp-pty 返回 prompt RPC 响应：
  {"jsonrpc":"2.0","id":3,"result":{}}
  ↓
cc-connect: EventResult(Done=true) → finalize IM 消息
  ↓
飞书 API → 用户看到最终结果
```

### 7.3 @term 命令处理

@term 命令作为普通 `session/prompt` 进入，由 acp-pty 内部解析：

```
用户: "@term ctrl-c"
  ↓
session/prompt(sessionId, prompt=[{type:"text", text:"@term ctrl-c"}])
  ↓
CommandParser::parse("@term ctrl-c") → Command::CtrlC
  ↓
session.rs: send_signal(SIGINT) 到 PTY child
  ↓
session/update 通知: "SIGINT sent"
  ↓
session/prompt RPC 返回: {}
```

```
用户: "@term stop"
  ↓
session/prompt → CommandParser → Command::Stop
  ↓
session.rs: kill PTY child, drop session
  ↓
session/update 通知: "shell terminated"
  ↓
session/prompt RPC 返回: {}
```

### 7.4 输出读取后台循环

```rust
async fn output_read_loop(session_id: SessionId, reader: PtyReader, buffer: OutputBuffer) {
    let mut buf = [0u8; 1024];
    loop {
        select! {
            n = reader.read(&mut buf) => {
                match n {
                    Ok(0) => break,
                    Ok(n) => buffer.append(&buf[..n]),
                    Err(_) => break,
                }
            }
            chunk = buffer.next_flush() => {
                let text = strip_ansi(&chunk);
                let text = truncate_if_needed(&text, 8192);
                send_notification("session/update", json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": text}
                    }
                }));
                // 通知 PromptTracker：有新输出，重置静默计时器
                prompt_tracker.notify_output();
            }
        }
    }
    sessions.remove(&session_id);
    send_notification("session/update", json!({
        "sessionId": session_id,
        "update": {
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": "shell exited"}
        }
    }));
}

// session/prompt 的返回时机由 PromptTracker 控制
async fn handle_session_prompt(session_id, prompt_text) -> RpcResult {
    let session = sessions.get(&session_id)?;
    session.write_to_pty(format!("{}\n", prompt_text));

    // 等待输出稳定：300ms 静默 或 30s 硬上限
    let tracker = session.prompt_tracker.clone();
    tracker.wait_for_settle(
        silence: Duration::from_millis(300),
        hard_limit: Duration::from_secs(30),
    ).await;

    Ok(json!({}))
}
```

`PromptTracker` 的逻辑：
- `notify_output()` — output_read_loop 每次 flush 后调用，重置 300ms 静默计时器
- `wait_for_settle()` — 阻塞直到 300ms 无新 notify_output，或 30s 硬上限到达
- `@term ctrl-c` / `@term stop` 命令直接完成 tracker，立即返回 RPC

## 8. ACP 协议实现

acp-pty 实现 ACP（Agent Client Protocol）的最小子集。传输层为 **newline-delimited JSON-RPC 2.0 over stdio**。

### 8.1 acp-pty 需要处理的 RPC 方法

| 方向 | 方法 | 必须实现 | 说明 |
|---|---|---|---|
| cc→term | `initialize` | ✅ | 握手，声明能力 |
| cc→term | `session/new` | ✅ | 创建 PTY 会话 |
| cc→term | `session/prompt` | ✅ | 用户输入写入 PTY |
| cc→term | `session/load` | ✅ | 始终返回 error（PTY 不可恢复） |
| cc→term | `session/list` | ✅ | 列出活跃 PTY 会话（替代 @term sessions 命令） |
| cc→term | `session/set_mode` | 忽略 | 返回空 `{}` |
| term→cc | `session/update` | ✅ | PTY 输出通知（notification，无 id） |

### 8.2 请求/响应示例

**initialize**

```json
// → stdin
{"jsonrpc":"2.0","id":1,"method":"initialize",
 "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"cc-connect"}}}

// ← stdout
{"jsonrpc":"2.0","id":1,"result":{
  "protocolVersion":1,
  "agentCapabilities":{
    "loadSession":false,
    "sessionCapabilities":{}
  }
}}
```

`loadSession: false` 告诉 cc-connect 不要尝试 `session/load`。

**session/new**

```json
// → stdin
{"jsonrpc":"2.0","id":2,"method":"session/new",
 "params":{"cwd":"/Users/melo/project","mcpServers":[]}}

// ← stdout
{"jsonrpc":"2.0","id":2,"result":{
  "sessionId":"term-a1b2c3d4"
}}
```

**session/prompt**

```json
// → stdin
{"jsonrpc":"2.0","id":3,"method":"session/prompt",
 "params":{"sessionId":"term-a1b2c3d4",
           "prompt":[{"type":"text","text":"ls -la"}]}}

// ← stdout (notification, 异步，可多条)
{"jsonrpc":"2.0","method":"session/update",
 "params":{"sessionId":"term-a1b2c3d4","update":{
   "sessionUpdate":"agent_message_chunk",
   "content":{"type":"text","text":"total 128\ndrwxr-xr-x ..."}
 }}}

// ← stdout (RPC 响应，表示本轮处理结束)
{"jsonrpc":"2.0","id":3,"result":{}}
```

**关键**：`session/prompt` 的 RPC 响应在 PTY 输出稳定后才返回（300ms 静默超时 或 30s 硬上限）。中间的输出通过 `session/update` notification 异步推送（无 `id` 字段）。cc-connect 收到 notification 后实时编辑 IM 消息。RPC 返回后 cc-connect 触发 `EventResult(Done=true)` finalize IM 消息。

**session/load（始终失败）**

```json
// → stdin
{"jsonrpc":"2.0","id":4,"method":"session/load",
 "params":{"sessionId":"term-a1b2c3d4","cwd":"/Users/melo/project","mcpServers":[]}}

// ← stdout
{"jsonrpc":"2.0","id":4,"error":{
  "code":-32600,"message":"PTY sessions cannot be restored"
}}
```

cc-connect 收到错误后会 fallback 到 `session/new`。

### 8.3 session/update 的 update 类型

acp-pty 只使用一种 update 类型：

| sessionUpdate | 用途 | 映射到 cc-connect 事件 |
|---|---|---|
| `agent_message_chunk` | PTY 输出文本 | `EventText` → 流式编辑 IM 消息 |

不需要实现 `tool_call`、`tool_call_update`、`plan` 等——acp-pty 没有工具调用概念。

### 8.4 stderr（日志）

acp-pty 的日志写 stderr，不干扰 stdin/stdout 协议通道。cc-connect 可选择捕获或丢弃。

## 9. 安全设计

### 9.1 攻击面分析

| 威胁 | 缓解 |
|---|---|
| 未授权用户执行命令 | cc-connect 侧 allow_from + admin_from 拦截，acp-pty 侧 cwd 白名单 |
| PTY 逃逸 | portable-pty 在独立进程中运行，不共享 acp-pty 的 fd |
| 环境变量泄露 | spawn_pty 使用最小化 env，不继承 acp-pty 的全部环境 |
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

| 能力 | cc-connect 负责 | acp-pty 负责 |
|---|---|---|
| IM 协议适配 | ✅ 7+ 平台 adapter | - |
| SessionKey 路由 | ✅ `{platform}:{chatID}:{userID}` | 不可见，cc-connect 内部 |
| ACP 会话管理 | ✅ 发起 initialize/session/new/prompt | ✅ 响应 RPC，管理 sessionId |
| 消息归一化 | ✅ `core.Message` → ACP prompt | ✅ 解析 JSON-RPC 2.0 |
| allow_from 过滤 | ✅ 平台层静默丢弃 | - |
| admin 白名单 | ✅ admin_from 项目层校验 | -（不重复做） |
| target 绑定 | - | ✅ cwd → shell 映射 |
| 流式消息编辑 | ✅ 读取 session/update → PreviewStarter | ✅ 发送 session/update notification |
| 消息分片 | ✅ 4000 rune 切割 | - |
| 输出缓冲 | - | ✅ 300ms/4KB 聚合 |
| ANSI 清除 | - | ✅ strip_ansi_escapes |
| PTY 管理 | - | ✅ portable-pty |
| 会话生命周期 | - | ✅ start/stop/timeout/reap |
| 会话恢复 | ✅ 尝试 session/load | ✅ 返回 error，触发 session/new |

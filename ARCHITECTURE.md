# shell-acp 架构文档

> ACP 协议到本地 PTY 的翻译层 — cc-connect 生态的 shell 后端
>
> 基于 cc-connect 源码研究（Go, `/root/cc-connect`）推导

## 1. 与 cc-connect 的关系

shell-acp **不重新实现 IM 协议适配**。cc-connect 已经完成了所有脏活：

```
飞书 / Telegram / Slack / Discord / 企业微信 / LINE / DingTalk / QQ
                        ↓
              cc-connect (Go, 已有)
          ┌─────────────┼─────────────┐
          ↓             ↓             ↓
     Claude Code   shell-acp  其他 backend
     (现有后端)      (本项目)
```

cc-connect 通过 **ACP（Agent Client Protocol）** 与后端通信——它启动一个子进程，用 stdin/stdout 交换 **JSON-RPC 2.0** 消息。shell-acp 要做的就是实现 ACP 协议的最小子集，作为 `type = "acp"` agent 接入 cc-connect。

### cc-connect 已经帮我们做了的事

| 能力 | cc-connect 如何实现 | shell-acp 是否需要关心 |
|---|---|---|
| IM 协议适配 | 每个平台一个 adapter（WebSocket/Webhook/Polling） | 不需要 |
| 消息归一化 | 所有平台 → `core.Message` 统一结构 | 不需要，只收 ACP JSON-RPC |
| SessionKey 路由 | `{platform}:{chatID}:{userID}` 格式 | 不直接可见，cc-connect 内部路由；shell-acp 用 ACP sessionId |
| 流式消息编辑 | `PreviewStarter` / `UpdateMessage` 接口 | 不需要，cc-connect 处理 |
| 消息分片 | 4000 rune 上限自动切割 | 不需要，cc-connect 处理 |
| 权限第一层 | `allow_from` 在平台层静默丢弃 | 不需要 |

### shell-acp 只需要实现

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

### shell-acp 的消息入口（本项目实现）

shell-acp 是 cc-connect 的子进程，通过 ACP（JSON-RPC 2.0 over stdio）通信。

```
cc-connect → shell-acp stdin: JSON-RPC request
  ↓
逐行读取，按 JSON-RPC 2.0 分发：
  ├── "initialize"       → 握手，返回能力声明
  ├── "session/new"      → 创建 PTY 会话
  ├── "session/load"     → 恢复 PTY 会话（V1 直接失败，触发 session/new）
  ├── "session/prompt"   → 用户输入写入 PTY
  └── "session/list"     → 列出活跃会话
  ↓
shell-acp → cc-connect stdout: JSON-RPC response + notifications
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

### shell-acp 的会话映射

cc-connect 的 SessionKey（`{platform}:{chatID}:{userID}`）在 ACP 层不直接暴露给后端。shell-acp 通过 ACP 的 **sessionId** 管理会话：

```rust
type SessionId = String;  // shell-acp 自己生成，返回给 cc-connect

struct SessionRouter {
    sessions: DashMap<SessionId, LocalTerminalSession>,
}
```

**两层 session 的关系**：

```
cc-connect 侧                          shell-acp 侧
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
- `session/new` → **立即创建 PTY 并 spawn shell**，返回 sessionId。用户第一条消息直接写入 PTY，无需 `@shell start`
- `session/prompt` → 查找 sessionId 对应的 PTY，写入用户输入
- `session/load` → V1 始终返回错误（PTY 不可跨进程恢复），cc-connect 会 fallback 到 `session/new`
- 用户发 `@shell stop` → 作为普通 prompt 进入，shell-acp 内部解析后销毁 PTY 并清理 session
- cc-connect 重启 → 子进程被杀，所有 PTY 随之销毁；cc-connect 重新 spawn shell-acp 并调 `session/new`

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

### shell-acp 的输出缓冲（本项目实现）

PTY 输出与 AI 输出有本质区别：PTY 是**连续字节流**（每个字符都可能触发一次 read），AI 是**离散事件流**（EventText 粒度已经是词/句级别）。所以缓冲策略需要不同：

```
PTY stdout (连续字节流)
  ↓
TermRenderer (term.rs)              // 无头终端模拟器(vt100),非 strip
  ├── parser.process(bytes)         // 喂字节,维护屏幕网格
  └── 每 100ms:
      ├── take_completed()          // 发射光标行以上"已完成"的行
      │     · \r 覆盖/进度条 → 折叠为最终态
      │     · 颜色/光标控制 → 被模拟器解释(纯文本输出,丢色)
      │     · ZLE 重绘 → 渲染为最终行,无乱码
      └── current_line()            // 光标行(提示符),仅用于判定本轮结束,不发送
  ↓
redact(marker) + strip_pending_echo // 抹掉哨兵提示符 + 丢掉命令回显行
  ↓
truncate_if_needed()                // 超长输出截断,保留头尾
  ↓
session/update notification → cc-connect → IM
```

**为什么用真正的终端模拟器而不是 strip**:`strip_ansi` 只是删转义序列,处理不了 `\r` 覆盖(进度条)、光标定位、清屏、ZLE 重绘——删完会留下乱码。`vt100` 把字节流**渲染到一块屏幕网格**,我们读回渲染后的纯文本,这些都被正确处理。

**与 cc-connect 的分工**：
- shell-acp 负责：终端模拟渲染、判断"本轮输出结束"、语义截断
- cc-connect 负责：消息分片（4000 rune）、流式预览编辑、平台 API 调用

**为什么不在 shell-acp 做消息分片**：cc-connect 已经做了，而且它知道各平台的具体限制。shell-acp 只需要输出合理大小的文本块（≤ 8KB），cc-connect 会处理剩下的。

### session/prompt 的 RPC 返回时机

cc-connect 把 `session/prompt` RPC 返回当作**本轮结束信号**（触发 `EventResult(Done=true)`，finalize IM 消息）。所以 shell-acp 不能立即返回,必须等 PTY 输出稳定后再返回。本轮结束有三个触发条件,**任一满足即返回**:

1. **提示符匹配(快路径)** — 输出末尾出现已知提示符,立即返回。已知提示符有两种:
   - **shell 提示符(哨兵)** — 会话启动时 shell-acp 注入了一个唯一的哨兵 PS1(见 §9.2 shell 集成),普通命令走这条;它是**确定性精确匹配**,不受 oh-my-zsh / 主题 / 颜色影响。匹配到时还会清掉已学习的 REPL 提示符(说明已回到 shell)。哨兵会从展示给用户的输出里**抹掉**。
   - **学习到的 REPL 提示符** — 见下条。匹配到 `python3` 的 `>>> `、`mysql>`、`node >` 等时立即返回。
2. **静默兜底 + 提示符学习** — 提示符还没学到时,输出连续 `settle_idle_ms`(默认 3000ms)无新数据则返回,**并把这次的末行学习为 REPL 提示符**,于是第二条起的同类命令就能走快路径。窗口必须**大于持续命令的出行间隔**(`ping` 约 1s/行、`watch` 默认 2s),这样 `ping` / `tail -f` / `watch` 这类**持续流**才能不断重置计时器、保持本轮存活、实时刷,而不是出一行就被收尾。
3. **硬上限** — 超过 `settle_hard_limit_secs`(默认 120s)强制返回。

```
write_to_pty("ls -la\n")
  ↓
TermRenderer 持续渲染 PTY 字节流
  ↓ 每次 flush → 发送 session/update notification（流式推送）
  ↓ 每次 flush → 末行匹配提示符则 force_complete；否则记下末行 + notify_output() 重置静默计时器
  ↓
满足任一结束条件（提示符匹配 / 3000ms 静默 / 120s 硬上限）
  ↓ 若按静默结束 → 把末行学习为 REPL 提示符
返回 session/prompt RPC response: {"jsonrpc":"2.0","id":3,"result":{}}
  ↓
cc-connect: EventResult(Done=true) → finalize IM 消息
```

**安全阀：硬上限默认 120 秒**。长命令（`cargo build`、`make`）或不会自己停的持续流（`ping`、`tail -f`）可能持续输出数分钟。超过硬上限后强制返回 RPC，后续输出继续通过 notification best-effort 推送（cc-connect 可能显示为新消息）。持续流建议用有界形式(`ping -c 6`)或 `@shell ctrl-c` 收尾。

| 场景 | 行为 |
|---|---|
| 普通 shell 命令（ls, pwd, cat） | 输出完毕、shell 提示符重现 → 提示符匹配，立即返回（~0.1s） |
| 进入 REPL（python3, node, mysql） | 首次靠静默 3000ms 返回并学习 `>>> ` 等;之后同类命令提示符匹配立即返回 |
| 持续流（ping, tail -f, watch） | 每行间隔 < 3000ms,不断重置计时器 → 本轮存活、实时流式推送,直到命令结束/ctrl-c/120s 硬上限 |
| 长命令（cargo build） | notification 流式推送,结束时 shell 提示符重现立即返回;否则 120s 硬上限强制返回 |
| 无输出命令（cd, export） | 提示符立即重现 → 匹配返回 |

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

### shell-acp 的权限模型（本项目实现）

#### 权限分工：cc-connect 管人，shell-acp 管路由

cc-connect 已经有两道权限门：
1. `allow_from` — 平台层，过滤非白名单用户（静默丢弃）
2. `admin_from` — 项目层，过滤非管理员（回复错误）

能到达 shell-acp stdin 的消息，**用户身份已经被 cc-connect 校验过了**。shell-acp 不需要重复做 user 白名单。

shell-acp 只需要做一件事：**cwd → target 绑定**（决定这个会话用哪个 shell）。

```
消息经过 cc-connect allow_from + admin_from 校验后
  ↓
session/new(cwd) 到达 shell-acp
  ↓
cwd → target 绑定
  ├── session/new 的 cwd 参数来自 cc-connect 配置
  ├── shell-acp 校验 cwd 是否在允许列表中
  ├── 匹配 config.targets.*.cwd → 决定 shell 类型
  ├── 不匹配 → 返回 JSON-RPC error
  └── 匹配 → 创建 PTY
  ↓
放行 → 创建 PTY 会话
```

```toml
# shell-acp config.toml

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
command = "/usr/local/bin/shell-acp"
args = ["--config", "/etc/shell-acp.toml"]
work_dir = "/Users/melo/project"  # 传给 session/new 的 cwd
```

#### 权限分层总结

| 层 | 谁负责 | 做什么 | 失败行为 |
|---|---|---|---|
| allow_from | cc-connect | 过滤非白名单用户 | 静默丢弃 |
| admin_from | cc-connect | 过滤非管理员 | 回复 "Admin privilege required" |
| target 绑定 | shell-acp | cwd → shell 映射 | JSON-RPC error |

**设计理由**：不在 shell-acp 重复做 user 白名单，避免 4 层权限检查的维护负担。cc-connect 的 `admin_from` 已经足够严格——配置时不用 `"*"` 通配符即可。安全边界在 cc-connect 层就闭合了。

## 6. 核心模块设计

```
shell-acp/
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
│   │   └── CommandParser     # @shell stop/ctrl-c
│   ├── session.rs           # PTY 会话管理
│   │   ├── LocalTerminalSession
│   │   ├── spawn_pty()       # portable-pty 启动 shell
│   │   └── write_to_pty()    # 用户输入写入 stdin
│   ├── buffer.rs            # 输出缓冲
│   │   ├── TermRenderer      # vt100 无头终端模拟
│   │   ├── take_completed()  # 渲染已完成行(色/\r/ZLE 已解释)
│   │   └── PromptTracker     # 提示符匹配(含学习) / 3000ms 静默 / 120s 硬上限，控制 RPC 返回时机
│   ├── target.rs            # target 绑定
│   │   └── resolve_target()  # cwd → TargetConfig 映射
│   └── config.rs            # TOML 配置加载
│       ├── Config
│       └── TargetConfig
├── config.example.toml
└── Cargo.toml
```

## 7. 数据流详解

### 7.1 首次连接：initialize + session/new

```
cc-connect spawn shell-acp 子进程
  ↓
cc-connect stdin →
  {"jsonrpc":"2.0","id":1,"method":"initialize",
   "params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{}}}

shell-acp stdout →
  {"jsonrpc":"2.0","id":1,"result":{
    "protocolVersion":1,
    "agentCapabilities":{"loadSession":false,"sessionCapabilities":{}}
  }}
  ↓
cc-connect stdin →
  {"jsonrpc":"2.0","id":2,"method":"session/new",
   "params":{"cwd":"/Users/melo/project","mcpServers":[]}}

shell-acp 内部：
  ↓ target.rs: resolve_target("/Users/melo/project") → target=dev, shell=zsh
  ↓ session.rs: spawn_pty(zsh, cwd="/Users/melo/project")
  ↓ sessions.insert("term-a1b2c3", LocalTerminalSession {...})
  ↓ tokio::spawn(output_read_loop("term-a1b2c3"))

shell-acp stdout →
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
shell-acp 内部：
  ↓ acp.rs: 解析 session/prompt
  ↓ router.rs: 查找 sessions["term-a1b2c3"]
  ↓ CommandParser: "ls -la" 不是 @shell 命令 → 普通输入
  ↓ session.rs: write_to_pty("ls -la\n")
  ↓
PTY 执行 ls -la → stdout 输出字节流
  ↓
term.rs: TermRenderer 渲染（vt100 屏幕网格，100ms 发射已完成行）
  ↓ 颜色/\r/光标控制由模拟器解释为纯文本
  ↓
shell-acp stdout → session/update 通知（可能多条，流式推送）：
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
提示符重现，或 3000ms 静默无新输出（或 120s 硬上限）
  ↓
shell-acp 返回 prompt RPC 响应：
  {"jsonrpc":"2.0","id":3,"result":{}}
  ↓
cc-connect: EventResult(Done=true) → finalize IM 消息
  ↓
飞书 API → 用户看到最终结果
```

### 7.3 @shell 命令处理

@shell 命令作为普通 `session/prompt` 进入，由 shell-acp 内部解析：

```
用户: "@shell ctrl-c"
  ↓
session/prompt(sessionId, prompt=[{type:"text", text:"@shell ctrl-c"}])
  ↓
CommandParser::parse("@shell ctrl-c") → Command::CtrlC
  ↓
session.rs: send_signal(SIGINT) 到 PTY child
  ↓
session/update 通知: "SIGINT sent"
  ↓
session/prompt RPC 返回: {}
```

```
用户: "@shell stop"
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
async fn output_read_loop(session_id: SessionId, byte_rx: Receiver, renderer: TermRenderer) {
    loop {
        select! {
            bytes = byte_rx.recv() => {
                match bytes {
                    Some(data) => renderer.feed(&data),  // 喂给 vt100 模拟器
                    None => break,
                }
            }
            _ = sleep(100ms), if renderer.pending() => {
                let text = renderer.take_completed();
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

    // 等待输出稳定：提示符匹配 / 静默兜底 / 硬上限
    let tracker = session.prompt_tracker.clone();
    tracker.wait_for_settle(
        idle_window: Duration::from_millis(settle_idle_ms),      // 默认 800
        hard_limit:  Duration::from_secs(settle_hard_limit_secs), // 默认 120
    ).await;

    Ok(json!({}))
}
```

`PromptTracker` 的逻辑：
- `set_marker()` — 会话启动时设入注入的哨兵 PS1,作为快路径精确匹配判据;并用于从输出中抹掉哨兵
- `output_ends_with_prompt()` → `force_complete()` — 输出末尾匹配提示符则立即结束本轮
- `notify_output()` — output_read_loop 每次 flush 后调用，**重置静默计时器**
- `wait_for_settle()` — 阻塞直到以下任一:提示符匹配、连续 `idle_window` 无新 `notify_output`、或 `hard_limit` 硬上限
- `@shell ctrl-c` / `@shell stop` 命令直接完成 tracker，立即返回 RPC

## 8. ACP 协议实现

shell-acp 实现 ACP（Agent Client Protocol）的最小子集。传输层为 **newline-delimited JSON-RPC 2.0 over stdio**。

### 8.1 shell-acp 需要处理的 RPC 方法

| 方向 | 方法 | 必须实现 | 说明 |
|---|---|---|---|
| cc→term | `initialize` | ✅ | 握手，声明能力 |
| cc→term | `session/new` | ✅ | 创建 PTY 会话 |
| cc→term | `session/prompt` | ✅ | 用户输入写入 PTY |
| cc→term | `session/load` | 防御性 | 始终返回 error（`loadSession:false` 已告知 cc-connect 不调用，此为兜底） |
| cc→term | `session/list` | ✅ | 列出活跃 PTY 会话（替代 @shell sessions 命令） |
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

**关键**：`session/prompt` 的 RPC 响应在 PTY 输出稳定后才返回（提示符匹配 / 3000ms 静默 / 120s 硬上限,任一触发）。中间的输出通过 `session/update` notification 异步推送（无 `id` 字段）。cc-connect 收到 notification 后实时编辑 IM 消息。RPC 返回后 cc-connect 触发 `EventResult(Done=true)` finalize IM 消息。

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

**session/list**

```json
// → stdin
{"jsonrpc":"2.0","id":5,"method":"session/list","params":{}}

// ← stdout
{"jsonrpc":"2.0","id":5,"result":{
  "sessions":[
    {"sessionId":"term-a1b2c3d4","cwd":"/Users/melo/project","title":"zsh"}
  ]
}}
```

返回所有活跃 PTY 会话。`title` 字段为 shell 类型。session 被 `@shell stop` 销毁后不再出现在列表中。

**session/prompt 对已销毁 session 的处理**

用户 `@shell stop` 后 shell-acp 销毁 PTY 并移除 session。但 cc-connect 仍缓存着旧 sessionId，后续用户消息会以该 sessionId 发来 `session/prompt`：

```json
// → stdin（session 已不存在）
{"jsonrpc":"2.0","id":6,"method":"session/prompt",
 "params":{"sessionId":"term-a1b2c3d4",
           "prompt":[{"type":"text","text":"ls"}]}}

// ← stdout
{"jsonrpc":"2.0","id":6,"error":{
  "code":-32600,"message":"session not found: term-a1b2c3d4"
}}
```

cc-connect 收到错误后会调 `session/new` 重新创建会话。用户无需手动操作。

### 8.3 session/update 的 update 类型

shell-acp 只使用一种 update 类型：

| sessionUpdate | 用途 | 映射到 cc-connect 事件 |
|---|---|---|
| `agent_message_chunk` | PTY 输出文本 | `EventText` → 流式编辑 IM 消息 |

不需要实现 `tool_call`、`tool_call_update`、`plan` 等——shell-acp 没有工具调用概念。

### 8.4 stderr（日志）

shell-acp 的日志写 stderr，不干扰 stdin/stdout 协议通道。cc-connect 可选择捕获或丢弃。

## 9. 安全设计

### 9.1 攻击面分析

| 威胁 | 缓解 |
|---|---|
| 未授权用户执行命令 | cc-connect 侧 allow_from + admin_from 拦截，shell-acp 侧 cwd 白名单 |
| PTY 逃逸 | portable-pty 在独立进程中运行，不共享 shell-acp 的 fd |
| 环境变量泄露 | ⚠️ 当前实现**继承父进程全部 env**(仅覆盖 HOME/TERM/LANG),见 §9.2 — 待办:收敛为白名单 |
| 输出注入 IM | vt100 模拟渲染为纯文本,控制序列被解释而非透传,防止 IM 端异常 |
| 资源耗尽 | 最大并发 session 数限制（默认 16），单 session 输出速率限制 |
| Shell 进程僵尸 | child process reaper：定期检查 + session 空闲超时自动 kill |

### 9.2 Shell 集成(干净启动 + 哨兵提示符)与环境

为了**鲁棒性**(不被 shell 启动期的交互式步骤卡住)和**确定性提示符检测**,`session.rs` 这样起 shell:

1. **不读 rc/profile 启动**,避免任何首次运行向导拦截 —— 尤其是没有 `~/.zshrc` 时 zsh 会跑 `zsh-newuser-install` 向导卡住:
   - zsh:`zsh -f -i`
   - bash:`bash --norc --noprofile -i`
   - 其他:`<shell> -i`
2. **自己补 source 用户 rc**,保留用户的 alias/PATH 等(向导已被上一步规避):
   - 如 `[ -f "$HOME/.zshrc" ] && source "$HOME/.zshrc"`
3. **注入唯一哨兵 PS1**(如 `__SHELLACP_<token>__`),作为确定性提示符判据;展示给用户时从输出里抹掉。zsh 还会清空 `precmd_functions`,bash 清空 `PROMPT_COMMAND`,防止主题动态改写提示符。此外,每轮还会**剥离命令自身的终端回显行**(`set_pending_echo` / `strip_pending_echo`)——用户在 IM 里发的命令已经可见,PTY 回显的那一行属于冗余。
4. **就绪门 + 看门狗**:哨兵首次出现前的输出(启动噪声/init 行)一律丢弃;**第一条命令在写入前会先等就绪**(`wait_until_ready`),否则 cc-connect 在 `session/new` 后立即发命令时,init 输出会和首条命令输出挤在一起被一起丢弃(表现为首条命令空输出 + 卡满静默窗口)。若 5s 内仍未见哨兵(集成失败或 shell 真卡在交互式提示),打 `warn` 日志并降级为直接展示原始输出,避免“静默无输出”。

环境变量:当前**继承父进程全部 env**,然后覆盖 `HOME` / `TERM=xterm-256color` / `LANG=en_US.UTF-8`(`session.rs`)。因为有真正的无头终端模拟器(`term.rs`)在解释字节流,这里**故意 advertise 一个有能力的终端**,让程序正常发颜色/光标/ZLE 序列,由模拟器渲染成干净文本(对比早期 `TERM=dumb`+strip 的规避法)。zsh 启动 init 仍 `unsetopt PROMPT_SP PROMPT_CR` 去掉“部分行 `%` 标记”。

> ⚠️ 注意:这与早期设计设想的“最小化 env 白名单”不同 —— 现状会把父进程的敏感变量(如 `AWS_*`、`GITHUB_TOKEN`)透传进子 shell。**收敛为白名单是一个待办的加固项。**

### 9.3 空闲超时

```toml
[session]
idle_timeout_secs = 1800       # 30 分钟无输入自动销毁
max_sessions = 16              # 全局最大并发会话
max_output_buffer = 65536      # 单次输出缓冲上限 64KB
settle_idle_ms = 3000          # 提示符未学到时,静默多久判定本轮结束(须>持续流出行间隔)
settle_hard_limit_secs = 120   # 单轮 RPC 返回的硬上限
```

## 10. 从 cc-connect 借鉴 vs 自己实现 — 总结

| 能力 | cc-connect 负责 | shell-acp 负责 |
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
| 输出渲染 | - | ✅ vt100 无头终端模拟 |
| 颜色/光标/\r | - | ✅ 模拟器解释(进度条折叠、ZLE 渲染) |
| PTY 管理 | - | ✅ portable-pty |
| 会话生命周期 | - | ✅ start/stop/timeout/reap |
| 会话恢复 | ✅ 尝试 session/load | ✅ 返回 error，触发 session/new |

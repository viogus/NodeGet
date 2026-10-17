# NodeGet × Arcane：PTY 载体的 Docker 隧道方案

> 状态：**方案稿 v1（待评审）**
> 前置阅读：[arcane-environment-provider.md](arcane-environment-provider.md)（主设计文档，D1-A/D1-B/D2-A 已实现并验证）
> 本文只解决一个问题：**在完全不改动 nodeget-agent 二进制的前提下，让 Arcane 纳管 agent 主机上的 Docker。**

---

## 0. 结论摘要

- 已实现的主设计（新增 `docker_tunnel` 任务类型）**必须升级 agent**：旧 agent 反序列化 `task_event_type` 时会直接失败，任务根本不会执行。
- 但旧 agent 已经具备唯一可用的替代原语：**WebShell / PTY**（`web_shell` 任务 + `/terminal` 拨入 + 每任务令牌反查）。把它当传输层，只用 server 侧改动就能拿到一条「到 agent 主机任意字节流」的通道。
- 方案本体：server 建 `web_shell` 任务 → agent 拨入 `/terminal` → server 附着该会话 → 用 `stty raw -echo` + `socat - UNIX-CONNECT:<docker.sock>` 把 PTY 变成**透明二进制管道** → 之后复用现有 D1-A/D1-B 的字节泵与控制面。
- 本机已用 pty 实验证明透明性成立（见 §10.1），并暴露两个必须处理的坑：**短写**与**握手前的回显噪音**。
- 代价集中在三点：① agent 主机需要 `socat`；② 受 `WEBSHELL_MAX_SESSIONS = 8` 硬上限约束；③ **PTY 无法传递半关闭（FIN）**，交互式 stdin 语义要降级。
- 建议实施形态：**双载体共存**（`carrier = "task" | "pty"`），新 agent 走 `task`、旧 agent 走 `pty`，共用同一套泵与控制面。

---

## 1. 约束与目标

| 项 | 内容 |
|---|---|
| 硬约束 | **不改动 nodeget-agent 二进制**（路由器 aarch64 OpenWrt 上不换 agent） |
| 允许改动 | `nodeget-server` 代码、JS worker 脚本、（必要时）Arcane fork |
| 目标 | Arcane 以 `docker-host` 环境纳管 agent 主机的 Docker，形态支持同机（unix socket）与跨机（TCP + 路径前缀 + Bearer） |
| 验收 | 与主设计 §10.2 / §10.3 相同的实测集合：`docker version`、容器/镜像列表、`docker run`（含 101 hijack 与 stdout/stderr）、`docker exec`、`/events` 长流、负例令牌被拒 |

---

## 2. 为什么必须换载体

旧 agent 的任务分发链路（`agent/src/tasks/mod.rs`）第一步就是 `serde_json` 解析 `task_event_type` → `TaskEventType`。枚举里没有 `docker_tunnel` 变体时：

1. 反序列化失败 → 整条任务消息被丢弃（不是「拒绝执行」，是「读不懂」）；
2. 因此任务类型方案对旧 agent **完全不可用**，且没有任何配置能绕过。

旧 agent 手里可用的原语，逐个筛一遍：

| 原语 | 时长 | 方向性 | 能否承载 Docker HTTP |
|---|---|---|---|
| `web_shell`（PTY） | 长驻（会话表 + 上限 8） | 双向字节流 | **可以**（需 raw 模式） |
| `execute` | 上限 1 分钟（超时 SIGTERM→SIGKILL 进程组） | 一次性，只有最终输出 | 不行 |
| `http_request` | 30 秒超时，响应 ≤ 64 MiB | 一次性 TCP 请求 | 不行（`/events` 永不结束） |
| `read_config` / `edit_config` | — | 单文件 | 不行 |
| `ping` / `tcp_ping` / `http_ping` | — | 探针 | 不行 |
| 主 JSON-RPC WebSocket | 长驻 | 请求/响应 JSON | 不行（要 agent 配合新方法） |

→ **只有 WebShell/PTY 满足「长驻 + 双向 + 字节流」。**

---

## 3. 载体选型：WebShell / PTY

### 3.1 可复用的既有机制（源码锚点）

- 任务载荷：`crates/ng-task/src/types/mod.rs:20-25` `WebShellTask { url: url::Url, terminal_id: uuid::Uuid }`；`url` 与 `terminal_id` **由调用方给出**，server 不生成。
- agent 侧展开：`agent/src/tasks/pty.rs:504-539` `parse_url()` —— 要求 scheme 为 `ws`/`wss`；当 path 为 `/auto_gen` 时用**任务 URL 的 scheme/host/port** 重写成 `/terminal?agent_uuid=..&task_id=..&task_token=..&terminal_id=..`，agent_uuid 取自 agent 自己的配置。
  - ⚠️ 展开式里是 `{scheme}://{host}:{port}`，**必须在配置里给显式端口**（例如 `wss://nodeget-controller.cdf.mba:443`），不要依赖默认端口推断。
- agent 侧会话：`pty.rs:105` `handle_pty_url`（去重池 + 10s 连接超时）、`:220` `handle_pty_session`（PTY ↔ WS 双向泵，二进制走 `Message::Binary`）。
- 服务端会话表：`crates/ng-terminal/src/terminal.rs:40-43` `TerminalState { sessions }`、`:47-52` `TerminalSessionKey { agent_uuid, terminal_id }`、`:59-69` `SessionSlots { tx_to_agent, rx_from_agent, task_token }`、`:34` `TERMINAL_CHANNEL_BUFFER_SIZE = 4096`。
- 拨入鉴权：`crates/ng-terminal/src/check_agent.rs:32` `check_agent(agent_uuid, task_token, task_id)`，`:65-71` 要求任务类型为 `WebShell`，否则 `PermissionDenied`。
- 用户侧鉴权：`crates/ng-terminal/src/auth.rs:33` `check_terminal_connect_permission`（`Scope::AgentUuid` + `Terminal::Connect`），**这是新载体唯一需要绕开/满足的检查**（见 D2）。
- 并发上限：`agent/src/tasks/mod.rs:108` `WEBSHELL_MAX_SESSIONS = 8` + `:111` 信号量 + `:441-446` 非阻塞 `try_acquire`（满了立即失败上报）。
- 开关：`crates/ng-config/src/config/agent.rs:99` `allow_web_shell`、`:56` `terminal_shell`。

### 3.2 与 `docker_tunnel` 载体的差异

| 维度 | `task` 载体（现状） | `pty` 载体（本方案） |
|---|---|---|
| agent 二进制 | 必须升级 | **不动** |
| agent 配置 | `allow_docker_tunnel` + `[docker_tunnel] socket_path` | 只需 `allow_web_shell = true`（**socket 路径由 server 下发**） |
| agent 主机额外依赖 | 无 | `socat`（或 `nc -U`） |
| 并发上限 | `max_tunnels`（可配，0=不限） | `WEBSHELL_MAX_SESSIONS = 8`（**硬编码，改不了**） |
| 半关闭传播 | 有（`HALF_CLOSE_CONTROL` 控制帧） | **无**（PTY 没有 FIN 语义，见 D5） |
| 吞吐 | 裸 socket 级 | 多一层 PTY（行规程 + 内核缓冲），略低 |
| 回连地址 | agent 从自身 `ws_url` 派生 `/docker-tunnel` | server 必须知道 agent 可达的 `scheme://host:port`（见 D7） |

---

## 4. 架构与数据流

```
Arcane manager
   │ ① docker-host 环境：unix://<socket_dir>/<uuid>.sock   或
   │                    tcp://<server>:23750/agent/<uuid> + Bearer
   ▼
nodeget-server ── [现有控制面，完全复用]
   │  · UnixListener（每 agent 一个，裸字节）
   │  · TCP + 路径前缀剥离 + Bearer 鉴权（hyper http1，含 101 升级透传）
   │  · pending 注册表 / 空闲回收 / 半关闭控制帧
   ▼ [载体适配层，本方案新增]
   │ ② create_task(web_shell, url=<agent_ws_base>/auto_gen, terminal_id=<随机>)
   │ ③ 附着自己 /terminal 的该会话（进程内 API）
   │ ④ 握手：stty raw -echo; printf NGREADY; exec socat - UNIX-CONNECT:<docker.sock>
   ▼ WSS（agent 拨入，长期存在）
nodeget-agent（旧二进制）
   │ ⑤ 已有 pty 任务：PTY ↔ WS 双向泵
   ▼ PTY（raw）
socat ⇄ /var/run/docker.sock
```

关键点：**载体只替换 ④ 之下那一段**；①②③ 之上的请求解析、鉴权、路径前缀、半关闭控制帧、空闲回收**一行都不用改**，因为它们本来就是面对一个抽象的「字节流」。

---

## 5. 关键设计决策

### D1 任务粒度：每本地连接一个 `web_shell` 任务

一条 PTY 就是一条串行字节流，无法承载多条并发 HTTP 连接。因此沿用现有 D1-A/D1-B 的模型：**Arcane 每建立一条连接（TCP）或每次 accept（unix），就申请一条隧道 = 一个 `web_shell` 任务**。

- 复用现有 `claim_tunnel()`（`server/src/docker_tunnel.rs`）的 pending 注册表思路，只是把「创建 `docker_tunnel` 任务」换成「创建 `web_shell` 任务 + 附着」。
- 直接后果：并发上限 = `WEBSHELL_MAX_SESSIONS = 8`（见 D6）。

### D2 附着方式：给 ng-terminal 增加进程内 attach API（推荐）

`handle_user`（`terminal.rs:323`）与我们要做的事只差一层 WebSocket：它取走 `rx_from_agent`、用 `tx_to_agent` 发送。新增：

```rust
// crates/ng-terminal/src/session.rs（新增）
pub struct TerminalAttachment {
    pub to_agent: mpsc::Sender<Vec<u8>>,   // 内部包成 Message::Binary
    pub from_agent: mpsc::Receiver<Vec<u8>>,
}
pub async fn attach_existing(
    agent_uuid: Uuid,
    terminal_id: Uuid,
    wait: Duration,
) -> anyhow::Result<TerminalAttachment>;
```

要点：
- `TerminalState` 改为进程内唯一实例（`router()` 里初始化进 `static TERMINAL_STATE: OnceLock<Arc<TerminalState>>`）。**注意这是「暴露已有的那一份」，不是再造第二份全局 state**（CONTRIBUTING 明令禁止再造）。
- `attach_existing` 需要**等待 agent 拨入**（任务创建后 agent 建立 WS 有延迟）：以 100 ms 步进轮询会话表，窗口取 `handshake_timeout_secs`，超时即回收任务并失败。
- 该 API **不走** `check_terminal_connect_permission`（调用方是 server 内部、已经在 `check_tunnel_agent` 等价物里做过任务归属校验）。

**备选 D2-B（不改 ng-terminal）**：server 用内部铸造的令牌（`Scope::AgentUuid(uuid)` + `Permission::Terminal(Terminal::Connect)`）作为 WS 客户端连自己的 `/terminal`。缺点是引入自连网络依赖、TLS 自签证书问题、令牌生命周期管理；不推荐。

### D3 握手：哨兵切分「回显噪音」与「真实数据」

PTY 在 raw 生效前会回显我们敲进去的命令行，shell 还可能有 banner / rc 输出。握手序列：

```sh
stty raw -echo; printf 'NGREADY'; exec socat - UNIX-CONNECT:/var/run/docker.sock
```

- server 端写入该行后，**丢弃 `NGREADY` 之前的所有字节**，之后一律视为隧道数据。
- 本机实测：raw 生效前的回显长度不固定（受 shell 提示符影响），所以哨兵是**必需**的，不能靠固定偏移量。
- 若 `socat` 不存在/路径错误，shell 会打印错误后退出 → PTY EOF → 立刻失败，可诊断（把 PTY 的残余输出记进 server 日志，脱敏后）。

### D4 raw 模式与短写

- `stty raw -echo` 必须与 `exec` 在同一条命令行，避免两条命令之间出现第二次行规程处理。
- **必须循环处理短写**：本机实测单次 `write()` 写 4096 B 只落 1022 B。载体层写 PTY 时按 ≤1 KiB 分块并检查返回值，`EAGAIN` 时让出（`tokio::io::AsyncWriteExt::write_all` 语义即可，前提是底层 writer 是 `AsyncWrite`）。
- 读方向按 8 KiB 读（与 agent 侧 `pty.rs` 的 8192 一致）。

### D5 半关闭：PTY 无法传播 FIN —— 语义降级

主设计的 D1-A/D1-B 依赖 `HALF_CLOSE_CONTROL` 把「客户端只关了写半边」与「隧道结束」区分开（这是 `docker run` 输出丢失那个 bug 的修复）。PTY 没有 FIN，**无法表达半关闭**。降级规则：

| 事件 | PTY 载体下的行为 |
|---|---|
| 本地客户端**全关** | 直接结束隧道（丢弃会话），语义正确 |
| 本地客户端**半关**（只 `SHUT_WR`） | 无法传播；该方向停发，等对端关闭或空闲回收兜底 |
| docker daemon 关闭连接 | `socat` 退出 → PTY 读侧 EOF → 结束隧道（正确） |
| 空闲回收 | `idle_timeout_secs`（docker-host 建议 0 会失去兜底；PTY 载体下**建议不要设 0**，给 60–300 s 兜底） |

对 Arcane 的影响面评估：Arcane 用的是 Go HTTP 客户端，正常请求-响应与 keep-alive 场景不涉及半关闭；只有**容器 attach/exec（101 升级）**那种「客户端不再发数据但仍在读」的场景语义会退化。**这一条必须在真实 Arcane 上回归**（§10.3），不能凭推演通过。

> 若实测发现退化不可接受，退路是把 Docker 流**帧化**（base64/长度前缀帧 + 显式 `half_close` 帧）。代价是吞吐下降与复杂度上升，作为 Plan B 保留。

### D6 会话上限：`WEBSHELL_MAX_SESSIONS = 8`

这是**只改 server 无法突破**的硬上限（在 agent 二进制里）。缓解手段：

1. **在 Arcane 侧收口**（Arcane 是我们自己的 fork，可以改）：给 docker-host 的 moby client 注入 `client.WithHTTPClient(&http.Client{Transport: &http.Transport{MaxConnsPerHost: N}})`，把并发连接压到 6 以下，留 2 条给 `/events` 与长流。
2. server 侧在名额不足时**快速失败并给出明确错误**（HTTP 503 + 可读原因），而不是让 Arcane 看到莫名的连接错误。
3. 明确文档化：PTY 载体适合「少量并发 + 常规 API 操作」，不适合大量并行请求。

### D7 配置：agent 可达地址从哪来

`web_shell` 任务的 `url` 由**创建方**给出，而 server 并不知道 agent 配置里的 `ws_url`。新增配置项：

```toml
agent_ws_base = "wss://nodeget-controller.cdf.mba:443"
```

- 必须带**显式端口**（见 §3.1 的展开式）。
- 若部署形态是同机（server 与 agent 同机），可用 `ws://127.0.0.1:<ws_port>`。
- 该地址与 agent 自己的 `ws_url` 指向同一入口，本质上是 server 的对外地址。

### D8 与现有载体的关系：双载体共存

```toml
carrier = "auto"   # "auto"（按 agent 能力探测，暂不支持）| "task" | "pty"
```

v1 建议只做**显式选择**（`"task"` 默认，保持现有行为；`"pty"` 用于旧 agent），不引入能力探测——server 无法可靠地知道 agent 版本。`"auto"` 留待将来用 agent 上报的版本号实现。

---

## 6. 配置结构

### 6.1 server（新增字段）

```toml
[docker_tunnel]
enable = true
carrier = "pty"                          # 新增："task"（默认，新 agent）| "pty"（旧 agent）
agent_ws_base = "wss://nodeget-controller.cdf.mba:443"   # 新增：仅 pty 载体需要
pty_docker_socket = "/var/run/docker.sock"               # 新增：仅 pty 载体需要，agent 主机上的 daemon
handshake_timeout_secs = 10                              # 新增：等待 agent 拨入 + 握手的总窗口
# 以下沿用现有字段
socket_dir = "/run/nodeget/docker"
tcp_listen = "0.0.0.0:23750"
path_prefix = "/agent"
require_token = true
agents = ["9a352a52-4804-433a-8f3c-e35fcc817ef2"]
idle_timeout_secs = 120
max_tunnels = 4
```

- 全部 `Option<T>` + `*_or_default()`，不加 `#[serde(default)]`（仓库约定）。
- `carrier = "pty"` 时 `agents` 白名单与 `require_token` 语义**完全不变**——安全边界仍在 server 侧。

### 6.2 agent（不改二进制，只加配置）

```toml
[[server]]
# …… 原有字段不动 ……
allow_web_shell = true      # 必需（否则 web_shell 任务被门禁拒绝）

# 无需新增任何 [docker_tunnel] 段：socket 路径由 server 在握手里下发
```

依赖：agent 主机安装 `socat`（OpenWrt：`opkg install socat`；Debian：`apt install socat`；或使用 `nc -U` 变体）。

---

## 7. 状态机与生命周期

```
idle ──(本地连接/Arcane 建连)──► creating_task ──► waiting_agent ──► attaching
                                        │                │              │
                                        │                │              ▼
                                        │                │          handshaking
                                        │                │              │
                                        ▼                ▼              ▼
                                     failed ◄────────────┴──────────► pumping
                                                                        │
                                              （任一侧 EOF / 全关 / 名额耗尽 / 空闲超时）
                                                                        ▼
                                                                     closing → idle
```

| 状态 | 进入条件 | 失败/超时后的动作 |
|---|---|---|
| `creating_task` | accept 到本地连接，名额可用 | 名额不足 → 503 或直接丢弃本地连接 |
| `waiting_agent` | 任务已创建 | 超 `handshake_timeout_secs` → 回收任务、清 pending |
| `attaching` | ng-terminal 出现该 `(agent_uuid, terminal_id)` 会话 | 同上 |
| `handshaking` | 已附着，写握手命令 | 未见哨兵即 EOF → 记录 PTY 残余输出、回收 |
| `pumping` | 见到 `NGREADY` | — |
| `closing` | EOF / 全关 / 空闲超时 | 丢弃会话（agent 侧随之回收 PTY 与 socat） |

---

## 8. 权限与安全边界

1. **PTY 载体把权限从「WebShell」放大到「Docker socket」= 宿主机 root**。这是既有风险的显式化：`allow_web_shell = true` 本来就等于给 server 一个 shell。因此：
   - `agents` 白名单必须收敛到确需纳管的机器；
   - server 侧日志绝不出现任务令牌（沿用现有脱敏）；
   - 隧道入口（unix socket 文件权限 0600 / TCP 的 Bearer）必须与现有实现保持一致。
2. **握手命令里的 socket 路径来自 server 配置**，不是 agent 提供的 → 不存在由 agent 侧决定路径的攻击面；但**要防命令注入**：路径必须做白名单校验（绝对路径、不含空白与 shell 元字符），否则 `pty_docker_socket` 会成为 server 配置里的 RCE 载体。
3. TCP 入口仍是**明文 HTTP + Bearer**（与主设计 §10.3 相同的告警），生产需 TLS 反代 / WireGuard / 内网。
4. 建议：PTY 载体下 `require_token` 保持 `true`，并在 server 日志里区分 `carrier=pty` 的连接，便于审计。

---

## 9. 实施计划

| 阶段 | 内容 | 预估 |
|---|---|---|
| P1 | `ng-terminal` 增加 `attach_existing` + 单一 `TERMINAL_STATE`；单测覆盖「等不到会话即超时」 | 0.5 天 |
| P2 | `server/src/docker_tunnel.rs` 抽出载体 trait（`create_tunnel_stream() -> impl AsyncRead+AsyncWrite`），现有 `task` 载体实现它 | 0.5 天 |
| P3 | `pty` 载体实现：任务创建、附着、握手、哨兵切分、短写处理、失败诊断日志 | 1 天 |
| P4 | 配置项（`carrier` / `agent_ws_base` / `pty_docker_socket` / `handshake_timeout_secs`）+ 文档 | 0.5 天 |
| P5 | 本机端到端（pty 载体 + 真实 Docker + 真实 Arcane）与路由器真实验证 | 1 天 |
| P6 | 半关闭退化的判定；若不可接受，启动 Plan B（帧化） | 待评估 |

合计约 **3.5 天**（不含 Plan B）。

---

## 10. 测试策略

### 10.1 已完成的先行实验（本机）

用 Python `pty` 复现同一条链路（`stty raw -echo; ...; exec <程序>`）：

| 用例 | 结果 |
|---|---|
| 输入方向：256 B 分块写入 4096 B（含全 `0x00`–`0xFF`） | 落盘逐字节一致 ✅ |
| 输出方向：同一份数据回读 | 载荷部分逐字节一致 ✅（前面混有命令行回显，需哨兵剔除） |
| 单次写入 4096 B | **只落 1022 B** ❌ → 必须处理短写 |
| 不做 `stty raw`（对照） | 出现 `\r\n` 转换、回显与截断 ❌ |

### 10.2 必须补的自动化用例

1. 握手成功/失败（socat 不存在、socket 路径错误、超时）各一条；
2. 短写路径：一次写入超过 1 KiB 的分块压力；
3. `WEBSHELL_MAX_SESSIONS` 打满时的行为（第 9 条连接必须得到明确失败，不是挂起）；
4. 负例：用非 `web_shell` 任务的令牌去连 `/terminal`（应被 `check_agent` 拒绝）；
5. 两个载体（`carrier = "task"` / `"pty"`）走同一套用例，结果应一致。

### 10.3 真实环境回归（照抄主设计 §10.2/§10.3 的清单）

- 路由器（OpenWrt + aarch64）上安装 `socat` 后跑通；
- `docker version` / 容器与镜像列表 / `docker run`（101 hijack + stdout/stderr）/ `docker exec`；
- **Arcane 真机**：建 docker-host 环境（unix 与 tcp 两种端点）→ `POST /test` 变 online → 容器列表与直连一致 → 打开容器日志/控制台（这是半关闭退化最可能暴露的地方）→ `/events` 长流持续 10 分钟不掉。

---

## 11. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| `WEBSHELL_MAX_SESSIONS = 8` | 并发一高就失败 | Arcane 侧 `MaxConnsPerHost` 收口；server 明确 503；文档化 |
| 半关闭不可传播 | attach/exec 交互语义退化 | §10.3 实测判定；不可接受则 Plan B 帧化 |
| PTY 输出队列在突发下丢字节 | 大响应损坏 | 读侧持续消费 + 端到端校验（对 dev 的 `_ping`/大量 JSON 做校验式回归） |
| agent 主机没有 `socat` | 隧道起不来 | 握手失败诊断日志 + 安装说明；备选 `nc -U` |
| 握手回显混入数据 | Docker 客户端解析失败 | 哨兵切分（已实测必需） |
| server 配置里的 socket 路径被注入 | RCE | 绝对路径 + 字符白名单校验 |
| PTY 会话泄漏（agent 侧不回收） | 占满 8 个名额 | 空闲回收 + `stop()` 时主动销毁会话 |

---

## 12. 未决问题

1. **`carrier` 默认值**：保持现有 `"task"`（用户必须显式选 `"pty"`），还是按部署场景自动？建议显式。
2. **半关闭是否降级可接受**：需要路由器 + 真实 Arcane 实测后再定；不可接受就走 Plan B（帧化），届时方案要重写第 5/10 节。
3. **是否顺带把 `docker-tunnel_describe` RPC 补上**（主设计 §7.2 的可选项）：PTY 载体下一键配对更需要它（因为 agent 侧没有 socket 路径可填）。
4. **JS worker 的边界**：控制面（describe / 健康 / 一键配对 / token 轮换）放 JS 还是放 Rust RPC？JS 迭代快，但权限判断需要复用 Rust 侧 `check_token_limit`；建议 JS 只做编排与展示，鉴权仍在 Rust。
5. **PTY 载体是否需要限速**：给单条隧道加带宽/速率上限，避免 `docker pull` 之类大流量把 PTY 打满。

---

## 附录 A：代码锚点速查

| 关注点 | 位置 |
|---|---|
| WebShell 载荷 | `crates/ng-task/src/types/mod.rs:20-25` |
| `/terminal` 路由与处理器 | `crates/ng-terminal/src/terminal.rs:94, 113, 131, 142-155, 174, 323` |
| 会话表结构 | `crates/ng-terminal/src/terminal.rs:34, 40-43, 47-52, 59-69` |
| agent 拨入鉴权 | `crates/ng-terminal/src/check_agent.rs:32, 65-71` |
| 用户侧权限 | `crates/ng-terminal/src/auth.rs:33` |
| agent 侧 URL 展开 | `agent/src/tasks/pty.rs:504-539` |
| agent 侧 PTY 会话 | `agent/src/tasks/pty.rs:105, 220` |
| 会话上限与信号量 | `agent/src/tasks/mod.rs:108, 111, 441-446` |
| agent 配置项 | `crates/ng-config/src/config/agent.rs:56, 99` |
| 隧道控制面（复用） | `server/src/docker_tunnel.rs`（pending 注册表、`claim_tunnel`、泵、TCP 反代、空闲回收） |
| 路由装配点 | `server/src/subcommands/serve.rs`（`.merge(crate::docker_tunnel::router())`，必须在 `.fallback()` 之前） |

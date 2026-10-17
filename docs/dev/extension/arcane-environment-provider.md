# NodeGet 作为 Arcane 的 Docker 环境提供者 —— 设计文档

> 状态：**评审稿 v3**（2026-10-14 再评估：Arcane fork 基线已从「未提交工作区」变为**已提交、与上游同步的稳定版本**，见 §1.2）。
> 本文只做设计，不含实现。
> 关键决策已确认（D1 = 同机 unix socket + 跨机 TCP 双形态；D2 = 新增 `docker_host_token`；D3 = 先做一次性创建）；
> 其余待决策项见 §11。
> 涉及两个仓库：`NodeGet`（本仓库）与 `Arcane` fork（`viogus/arcane`）。

---

## 0. 结论摘要

Arcane 的 agent 协议**不是**一条轻量隧道，而是「同一个 Go backend 以 `AGENT_MODE=true` 启动、在本地跑完整 Arcane HTTP API（238 个端点）」再由管理端反向代理整个 API 面。因此**原生实现 Arcane agent 协议等于重写 Arcane 后端**，不可取。

Arcane fork 里**已经有一套「docker-host 环境」**（已提交并稳定，见第 1.2 节）：环境的 `docker_host` 直接填一个 Docker API 端点（`tcp://` / `http://` / `https://` / `unix://` / `npipe://`），管理端**不经过任何 agent**，直接把该环境的请求在进程内打到那个 Docker daemon 上。

于是 NodeGet 的角色被大幅简化成一句话：

> **NodeGet 把「内网中某台 nodeget-agent 主机上的 `/var/run/docker.sock`」暴露成一个管理端可达的 Docker API 端点，Arcane fork 把它当一个普通的 docker-host 环境纳管。**

这样 Arcane 侧**几乎不需要为 NodeGet 写新协议**，`container / image / volume / network / project / swarm / system` 等全部现有 handler 与 DTO 直接复用；需要新增的只有「端点发现与鉴权」这一薄层。

**已确认的交付形态（两种都做）**：

| 部署形态 | `docker_host` 取值 | NodeGet server 侧入口 | 鉴权 |
|---|---|---|---|
| 同机 / 共享卷 | `unix:///run/nodeget/docker/<uuid>.sock` | 每 agent 一个 `UnixListener`，**裸字节对拷**（无 HTTP 解析） | socket 文件权限（可选 token） |
| 跨机 | `tcp://<nodeget-host>:23750/agent/<uuid>` | 单 TCP 监听 + 路径前缀，**逐请求剥前缀转发**（hyper http1） | `Authorization: Bearer <docker_host_token>` |

两种形态**共用同一套 agent 侧任务与 server 侧隧道内核**，只有 server 侧接入点不同；实施上先做同机（§9 Phase 1），再做跨机（Phase 2）。

---

## 1. 背景与现状

### 1.1 目标

让 Arcane 能够纳管**所有运行 nodeget-agent 的主机**的 Docker，且：

- 不要求这些主机开放任何入站端口（nodeget-agent 是纯出站 WebSocket 客户端，从不监听）；
- 不在这些主机上额外常驻 `arcane-agent` 容器；
- Arcane 侧复用现有 UI 与 API，不做「只读子集」。

### 1.2 Arcane fork 现状（**关键前提**）

**基线已稳定（2026-10-14 复核）**：fork `viogus/arcane` 的 `main` 工作区干净，docker-host 能力已经**全部提交**，成为 `v2.16.0` 之后的一组正常提交，并且**与上游完全同步（领先上游 7 个提交、落后 0 个）**。

| 项 | 值 |
|---|---|
| fork 路径 / 远端 | `/Users/cdf/Codes/Arcane`；`origin = git@github.com:viogus/arcane.git`，`upstream = git@github.com:getarcaneapp/arcane.git` |
| 当前 HEAD | `cec176c6c`（`git describe` = `backend/v2.16.0-17-gcec176c6c`），分支 `main`，工作区干净、无 stash |
| docker-host 主提交 | `5e1ca4873 feat(docker-host): add direct remote Docker environments with per-environment isolation`，**125 files changed, 8465 insertions(+), 551 deletions(-)** |
| 上游基线 | `upstream/main` = `5185d9c81`；已发布版本 tag `v2.16.0` = `ca358204d release: 2.16.0`（docker-host **不在**该 release 内，在 v2.16.0 之后的 17 个提交里） |
| fork 独有提交（7 个） | `5e1ca4873` docker-host、`4389affe3` events 按环境过滤、`d32e523a3` project 列表按环境范围、`048c4b156` 后端性能、`0aa082ca8` 前端性能、`cb466d6f6` project 列表按环境范围、`cec176c6c` dashboard 上报 docker-host 的 Docker 版本 |
| 发行通道 | `.github/workflows/release.yml:146-147` 构建并发布 `ghcr.io/${{ github.repository_owner }}/manager` 与 `/agent` → 该 fork 即 `ghcr.io/viogus/manager`、`ghcr.io/viogus/agent` |
| 迁移编号占用 | `backend/resources/migrations/{sqlite,postgres}/` 已到 **097**，且两套目录一致 → **098 可用于 `docker_host_token`** |
| 既有测试 | 全仓共 **32 个** `*_test.go` 命中 docker-host（`backend/internal/environment/dockerhost_test.go`、`dockerhost_client_cache_test.go`、`backend/internal/system/dockerhost_stats_test.go`、`backend/internal/activity/dockerhost_test.go`、`backend/internal/dashboard/dockerhost_test.go`、`backend/internal/swarm/dockerhost_test.go`、`backend/internal/job/dockerhost_test.go` 等） |

`docker_host` / `DockerHost` / `IsDockerHost` 在 `backend` + `types` + `frontend` 中共命中 **581 处、84 个文件（其中非测试文件 56 个）**。该能力已贯穿 container / image / volume / network / project / swarm / system / dashboard / diagnostics / job / updater / activity / webhook / settings / vulnerability 等全部服务，**不是半成品**。

> **本轮再评估的结论**：设计不变，风险显著下降。原先第 1.2 节写的是「60 个文件的未提交工作区」，那意味着设计随时可能被作者重写；现在是上游同步的干净提交，我们只需要为**自己新增的那一层**（`docker_host_token` + `WithHTTPHeaders`）负责，并跟随上游 release 做常规 rebase。Arcane 侧待改的东西**没有变多，反而更明确**——见 D2 与 §4.3 的精确改动面。

关键落点：

| 位置 | 内容 |
|---|---|
| `backend/resources/migrations/{sqlite,postgres}/094_add_environment_docker_host.sql` | `ALTER TABLE environments ADD COLUMN docker_host TEXT;` |
| `backend/internal/environment/dockerhost.go` | 端点校验与互斥规则 |
| `backend/internal/environment/model.go` | `Environment.DockerHost *string`、`IsDockerHost()`、`DockerHostURL()`（`LocalEnvironmentID = "0"` 在 `service.go:98`，manager 自身环境的保留 ID） |
| `backend/pkg/libarcane/envcontext/envcontext.go` | `Target{EnvironmentID, DockerHost}` 请求作用域 |
| `backend/internal/docker/service.go` | 按端点缓存的 Docker client 与 event bus |
| `backend/internal/middleware/environment.go` | docker-host 环境不再代理，改为在进程内服务 |
| `backend/internal/environment/inprocess.go` | `InProcessTargetContext` / `EnvironmentEventBus` |
| `backend/internal/settings/environment_overrides.go` + migration 096 | 每环境设置覆盖 |
| migration 095 / 097 | 卷备份策略、卷备份结果的每环境隔离 |

**校验规则**（`dockerhost.go:37-75`，`NormalizeDockerHost`）：

- 允许的 scheme：`tcp` `http` `https` `unix` `npipe`；
- **`ssh://` 被显式拒绝**（moby 的 ssh transport 需要本地 `ssh` 二进制与 home 目录，manager 镜像里没有）；
- 端点 URL **不允许内嵌凭据**（`parsed.User != nil` 直接报错，注释：*the manager does not forward them*）；
- `tcp/http/https` 必须带 host **和 port**；`unix/npipe` 必须是绝对 socket 路径。

**互斥规则**（`validateEnvironmentTargetInternal` / `applyDockerHostUpdateInternal`）：

- `docker_host` 与 `api_url` **二者必居其一且不可同时存在**；
- docker-host 环境 **不能** 有 `access_token`；
- docker-host 环境强制 `is_edge = false`；
- 清空 `docker_host` 会把环境交还给 agent 流程。

**健康与状态**（`children/health/service.go:46-78`、`:120-132`、`service.go:1289`、`:1328`）：

- `Probe` 对 docker-host 环境走 `probeDockerHostInternal` → `GetHostClient` + `Ping`，**10s 超时**，返回 `online` / `offline` / `error`；
- `updateEnvironmentStatus` 会把 **`pending` 且探测成功**的直连环境**自动提升为 `online`**（`service.go:1334-1342`，日志在 :1341）——即 **docker-host 环境创建即自配对，不需要任何 token 交换**；
- `registerHealthJob` 为每个环境注册周期健康任务，docker-host 环境因为「进程内服务」而使用**该环境自己的 scheduler namespace**；
- `TestConnection(ctx, id, customApiUrl *string)` 同时服务「保存前的连接测试」。

**请求路由**（`middleware/environment.go`）：请求路径命中 `/api/environments/{envID}/…` 且 `envID != localID` 时：

1. 先鉴权、再做**每环境授权**（`matcher.Lookup(method, suffix)`，**未映射的路由默认拒绝 / fail closed**，`Sudo` 权限集直接放行）；
2. 解析环境得到 `EnvironmentTarget{ApiURL, AccessToken, Enabled, DockerHost}`；
3. **`DockerHost != ""` 时不代理**，而是把 `envcontext.Target` 塞进请求 context 后 `next(c)` —— 请求由本进程的常规 handler 用该 daemon 的 client 处理；
4. 其它情况才走 agent/edge/direct 的 HTTP 代理。manager-local 路径同样会被打上 target，以便 handler 在进程内处理。

**Docker client 选项**（`backend/internal/docker/service.go`）：

```go
func dockerClientOptionsInternal(host string) []client.Opt {
	opts := []client.Opt{client.WithHost(host)}
	if strings.HasPrefix(strings.ToLower(strings.TrimSpace(host)), "https://") {
		opts = append(opts, client.WithTLSClientConfig("", "", ""))
	}
	return opts
}
```

> ⚠️ 这里是**唯一**注入隧道凭据的地方（`service.go:108-114`），目前**没有任何自定义 header**。两个必须一并改的调用方：
>
> - **`detectDockerAPIVersion`（`service.go:188-210`）复用了同一个 `dockerClientOptionsInternal`**，并用它自己的 `/_ping` 做 API 版本协商。也就是说 token 必须在**协商之前**就注入，否则一个要求鉴权的远端 daemon 会在「创建 client」这一步就 401，表现为「环境永远 pending、日志只说 negotiate Docker API version 失败」，而不是任何一眼能看出的鉴权错误。
> - **client 缓存以裸 endpoint 字符串为键**（`hostClients map[string]*client.Client`，`service.go:117-152`），而 `CloseHostClient(host)` 只在**端点变了**（`environment/service.go:423-424`）或**环境被删**（`:507-508`）时调用。于是「token 轮换、端点不变」会**继续使用旧 token 的 keep-alive 连接**。D2-A 必须把 token 纳入缓存键（或让 token 变更也走一次 `CloseHostClient`），并把这条负例补进 `dockerhost_client_cache_test.go` 那套测试。

**已知的 moby client v0.6.2 事实**（已核对模块源码，`github.com/moby/moby/client@v0.6.2`）：

- `ParseHostURL`（`client.go:380-399`）对 `tcp://` **保留 URL 的 path** 作为 `basePath`；
- `WithHost`（`client_options.go:128-140`）把该 path 写入 `c.basePath`；
- `getAPIPath`（`client.go:319-321`）用 `path.Join(cli.basePath, …)` 拼每个请求，`ping.go:112,129` 也显式拼接 —— 因此 **basePath 对全部端点（含 attach/exec 这类 hijack 请求）都生效**；
- `client.WithHTTPHeaders(map[string]string)`（`client_options.go:214`）**存在** —— 可以让 fork 给某个环境的 Docker client 挂上任意 header。

结论：`tcp://nodeget.example:23750/agent/<uuid>` 这种「单端口 + 路径前缀」的寻址方式**开箱得到 moby 支持**，且 fork 现有的 `NormalizeDockerHost` 也不会拒绝它（它只校验 host 与 port）。

### 1.3 NodeGet 现状（已按源码逐行核对）

| 事实 | 位置 |
|---|---|
| **没有插件系统**；`docs/dev/extension/index.md` 定义「扩展」= 外部程序复用现有任务 | `docs/dev/extension/index.md` |
| HTTP 框架 axum 0.8.9（features `tokio/ws/http1/query`），app 在 `serve.rs:164-166` 构造，RPC 服务以 `rpc_service.call(req)` 直接嵌入 `any(...)` 分支，不用 `Router::merge` | `Cargo.toml:34`、`server/src/subcommands/serve.rs:164-182, 233, 380-388` |
| **没有任何 axum 鉴权中间件**（全仓 `middleware::from_fn` / `.layer(` 零命中，只有 jsonrpsee 的 RPC middleware）→ 新裸 HTTP 路由必须像 WebDAV 那样**自己手工鉴权** | `server/src/rpc_timing.rs:8`、`serve.rs:136-142` |
| **已经存在 unix socket HTTP 监听**（`enable_unix_socket` / `unix_socket_path`，默认 `/var/lib/nodeget.sock`），且服务的是**同一个 axum app** → D1-A 的 bind/清理逻辑有现成范式可抄 | `crates/ng-config/src/config/server.rs:26,29`、`serve.rs:427-448, 1115-1131` |
| agent 是纯出站 WebSocket 客户端，**从不监听端口**（唯一 bind 是 UDP NTP） | `agent/src/ntp.rs:103` |
| 任务类型枚举 | `crates/ng-task/src/types/mod.rs:116-148` |
| WebShell：server 下发带 WS URL 的任务 → **agent 主动拨出** → 双向转发 PTY 字节 | `crates/ng-task/src/types/mod.rs:20,127`、`agent/src/tasks/mod.rs:170,262`、`agent/src/tasks/pty.rs:504-537` |
| 「长驻会话表」模板：`TerminalState{sessions: Arc<RwLock<HashMap<..>>>}` / `TerminalSessionKey{agent_uuid, terminal_id}` / `SessionSlots{tx_to_agent, rx_from_agent, task_token}`，channel buffer `4096` | `crates/ng-terminal/src/terminal.rs:34-69, 224-305` |
| 「拨入鉴权」模板：`check_agent(agent_uuid, task_token, task_id)` 查 task 表（`id` + `uuid` + `token` + `task_event_result is null`），**再校验 task 类型**，否则 "Terminal connection is only allowed for WebShell tasks" | `crates/ng-terminal/src/check_agent.rs:32-71` |
| **全仓无任何 unix socket 客户端代码**（`UnixStream` 零命中），也没有 proxy / socks / port-forward / tunnel 代码 | — |
| agent 的 tokio 已启用 `net, io-util, process, fs` → `tokio::net::UnixStream` **无需改 Cargo.toml** | `agent/Cargo.toml:20` |
| agent 已有 `tokio-tungstenite 0.29`（出站 WS，含 rustls `Connector`） | `agent/Cargo.toml:30` |
| **workspace 未声明 `hyper` 与 `tokio-util`** → D1-B（跨机 HTTP 反代）需要新增依赖 | 根 `Cargo.toml` |
| `HttpRequestTask` **不支持 unix socket**（`url::Url` + reqwest 默认 TCP 连接器） | `agent/src/tasks/http_request.rs` |
| agent 默认**原生安装**（install 脚本），不是容器 | `docs/guide/install/install-script.md` |
| server 监听 `ws_listener`（默认 `0.0.0.0:2211`）；TLS 时 ALPN 强制 http/1.1-only | `crates/ng-config/src/config/server.rs:20`、`serve.rs:450-464, 570` |
| 可枚举 agent 的 RPC 已存在 | `crates/ng-monitoring/src/rpc/agent_uuid/list_all.rs:24`、`crates/ng-monitoring/src/rpc/nodeget/list_all_agent_uuid.rs:55` |
| agent 连接白名单：`allow_*` 布尔 + `allow_task_type` 类型白名单（若 `Some` 且非空则**取代**全部布尔开关）；门禁是**穷尽 match、无 `_` 通配**，新增类型会编译报错直到补齐 | `crates/ng-config/src/config/agent.rs:78-124`、`agent/src/tasks/mod.rs:155-177, 426` |
| 权限模型里**没有** tunnel / proxy / port-forward 变体，也没有「给某个裸 HTTP 路由授权一个独立 token」的通用机制 | `crates/ng-core/src/permission/data_structure.rs:60-91, 165-176, 219-222` |

### 1.4 硬约束

1. **nodeget-agent 不能监听任何端口**（架构承诺，不能为了这个功能破例）。
2. Arcane 管理端**必须能主动连到某个端点**——NodeGet 侧必须提供。
3. Docker socket **等价于 host root**。隧道端点绝不能成为无鉴权的公网后门。
4. `Arcane` fork 的 docker-host 能力是本设计的地基，**不重写**。
5. NodeGet 的 `Exec`/`HttpRequest` 现有任务**不足以**承载这个功能（一个没有 unix socket 支持，一个只能单向短请求）。

---

## 2. 总体架构

```
                    ┌───────────────────────── Arcane manager (fork) ─────────────────────────┐
                    │  Environment row: docker_host = tcp://nodeget:23750/agent/<uuid>        │
                    │  DockerClientService.GetClient(ctx) ─→ GetHostClient(host) ──┐          │
                    │  handler(container/image/volume/…)  ← envcontext.Target 作用域 │          │
                    └───────────────────────────────────────────────────────────────┼──────────┘
                                                                                    │ Docker Engine API
                                                                                    │ (+ Authorization: Bearer <tunnel-token>)
                                                                                    ▼
   ┌──────────────────────────────── NodeGet server ─────────────────────────────────┐
   │  [docker_tunnel] listener  ── 路径前缀路由 ── 鉴权 ── 隧道复用表                  │
   └───────────────────────────────────┬─────────────────────────────────────────────┘
                                       │ 复用 NodeGet 既有出站长连接
                                       ▼
   ┌──────────────────────── NodeGet agent（内网，无入站端口） ─────────────────────┐
   │  出站隧道任务 ── 字节管道 ── /var/run/docker.sock                                │
   └─────────────────────────────────────────────────────────────────────────────────┘
```

**控制面**（发现与配对）与**数据面**（Docker API 字节流）分离：

- 数据面：Docker Engine API over NodeGet 隧道，逐字节透明；
- 控制面：NodeGet 的 agent 清单（RPC `agent-uuid_list_all`）→ Arcane 侧创建/更新环境。

### 2.1 为什么数据面是「透明字节 / 透明 HTTP」而不是「NodeGet 解析 Docker API」

Docker Engine API 有三类流量：

1. 普通 JSON 请求（`/containers/json`、`/images/json`…）；
2. **长流**（`GET /events`、`GET /containers/{id}/logs?follow=1`）；
3. **连接升级 / hijack**（`POST /containers/{id}/attach`、`/exec/{id}/start`，返回 `101` 后变成裸 TCP）。

第 3 类决定了**语义代理比字节代理危险得多**。凡是能透传字节的方案，三类流量一次性全支持。

### 2.2 为什么不做「NodeGet 也实现一套 Arcane agent 协议」

见第 0 节：那是重写 238 个端点 + 全部 DTO，且 Arcane 每次改前端字段都要跟进。docker-host 路线把这份成本降到接近零。

### 2.3 隧道帧格式与半关闭（实现后补充，已端到端验证）

隧道内的 WebSocket 帧只有两种语义：

| 帧 | 语义 |
|---|---|
| `Binary` | 原始字节，双向对拷，**不解析** |
| `Text` | 控制帧（JSON）。目前只有一种：`{"t":"half_close"}` |

**为什么必须有半关闭**：Docker CLI 在 `POST /containers/{id}/attach`（以及 `docker exec` 的 `/exec/{id}/start`）之后会调用 `shutdown(SHUT_WR)` 关闭本地连接的**写半边**，然后继续读 attach 流；Docker daemon 同样依赖半关闭来判断 stdin 结束（`docker run -i` 场景）。如果实现把「本地 socket 读返回 0」当成「整条隧道断开」，agent 会连带关闭自己的 docker socket 连接，daemon 之后写出的 attach/exec 输出将**全部丢失**——典型症状是 `docker run` / `docker attach` / `docker exec` 只收到 `101 UPGRADED` 和 `wait` 响应却没有任何容器 stdout/stderr，而 `docker version` / `docker info` / `docker logs`（纯请求-响应）一切正常。

因此隧道的状态机是**按方向**收尾，而不是按连接：

1. 任一侧本地 socket 读到 EOF → 发 `half_close` 控制帧，把该方向标记为完成，**不拆除隧道**；
2. 收到 `half_close` → 只对本地 socket 调用 `shutdown()`（关写半边），另一方向继续搬运；
3. 两个方向都完成 → 发 WebSocket `Close` 正常收尾；
4. 收到 `Close` / `None` / WS 错误，或命中空闲回收超时 → 立即结束整条隧道。

这条规则也解释了为什么数据面必须是**透明字节**而不是「请求-响应配对」：任何按请求-响应配对的代理都会在半关闭场景下丢数据。

**已实测通过的真实用例**（NodeGet server + agent + 真实 Docker daemon，见第 10 节）：`docker version`（keep-alive 多请求）、`docker run --rm alpine sh -c 'echo …; echo … >&2'`（attach hijack + stdout/stderr 分流）、`echo INPUT | docker run --rm -i alpine sh -c 'read line; echo GOT:$line'`（stdin 写 + 半关闭传递）、`docker run` 间隔输出（长流）、`docker create` + `docker start -a`、`docker exec` 进运行中容器（hijack）。

---

## 3. 关键设计决策

### D1. 寻址与监听方式

| 方案 | 形态 | 优点 | 缺点 |
|---|---|---|---|
| **D1-A（推荐，同机）unix socket / agent** | `docker_host = unix:///run/nodeget/docker/<uuid>.sock` | 纯字节拼接，**零 HTTP 感知**，三类流量天然支持；无端口；**鉴权 = 文件系统权限**（比 token 更强）；撤销 = 删文件 | 要求 Arcane manager 与 NodeGet server **同机或共享卷** |
| **D1-B（跨机）单端口 + 路径前缀** | `docker_host = tcp://nodeget:23750/agent/<uuid>` | 一个端口服务全部 agent；寻址清晰；moby 原生支持 basePath | 必须在 server 侧实现**带 keep-alive 剥离前缀**的 HTTP 代理（见下）；Rust 侧工作量最大 |
| **D1-C（跨机极简）每 agent 一个回环端口** | `tcp://nodeget:23751` | 纯字节拼接，实现最简单 | 端口数随 agent 增长；跨机要放行端口段；与 agent 的绑定关系要持久化 |

> **D1-B 的坑（务必注意）**：Docker SDK 会把 basePath 加到**每一个**请求上，所以 keep-alive 连接里的第 2、3 个请求同样带前缀。**不能只重写第一行然后裸拼接**——必须在整条连接上逐请求解析请求头、剥前缀、转发，并解析响应边界（`Content-Length` / `chunked` / 连接关闭）。跨机场景建议直接用 `hyper` 的 http1 codec 双向对接（配合 `hyper::upgrade` 处理 `101`），不要手写 framing。注意 workspace 目前**没有**声明 `hyper` / `tokio-util`，这一阶段要新增依赖。

> **D1-A 实现细节**：为每个启用隧道的 agent 绑一个独立的 `UnixListener`（路径 `<socket_dir>/<agent_uuid>.sock`），`accept` 之后**不做任何 HTTP 解析**，直接把该连接的字节与隧道对面 agent 的 docker socket 对拷。因此它**完全不经过 axum 路由**，也就自然绕开了「NodeGet 没有鉴权中间件」的问题（鉴权 = socket 文件权限）。bind 与清理可参照 `server/src/subcommands/serve.rs:1115-1131`（`bind_unix_listener` / `cleanup_unix_socket_file`），socket 文件权限 `0600` 加上属主/属组控制。
>
> **为什么 unix 端点可以「裸字节对拷」而不会有前缀问题**（已在 moby v0.6.2 源码确认）：`ParseHostURL`（`client.go:380-399`）**只为 `tcp://` 填 `basePath`**（`basePath = parsed.Path`），其余 scheme 一律 `Path: ""`；`WithHost`（`client_options.go:128-143`）则把 `hostURL.Path` 赋给 `c.basePath`。所以 `unix:///run/nodeget/docker/<uuid>.sock` 的 `addr` 是整条 socket 路径、**`basePath` 为空**，客户端发出的就是普通的 `GET /_ping`、`GET /v1.45/containers/json` —— 与直连真实 `/var/run/docker.sock` 完全一致。（`tcp://` 则相反，见上一条警告。）

**决策（已确认）：D1-A 与 D1-B 都做。** 同机/共享卷部署走 D1-A，跨机部署走 D1-B。

两种形态在 NodeGet 侧**共用同一套隧道内核**：agent 侧任务、隧道注册表、凭据与生命周期管理完全相同，**只有 server 侧的接入点不同**——

| 形态 | server 侧入口 | 是否需要 HTTP 解析 | 鉴权 |
|---|---|---|---|
| D1-A | 每 agent 一个 `UnixListener`，accept 后裸字节对拷 | **否** | socket 文件权限（+ 可选 token） |
| D1-B | 一个 TCP 监听 + 路径前缀，逐请求剥前缀转发（keep-alive 要处理） | **是**（http1 codec） | Bearer token（D2-A） |

**实施顺序**：先 D1-A 后 D1-B。理由不是「D1-A 更便宜」，而是 D1-A 能在一个**完全没有 HTTP framing** 的环境里把「任务投递 → 拨入鉴权 → 隧道建立 → 字节泵 → 断线回收 → 状态映射」这条链路验证透；随后叠加的 D1-B 只需要新增一层 HTTP 反代，隧道内核不动。反过来做会让 HTTP 反代的 bug 与隧道内核的 bug 混在一起难以定位。

### D2. 隧道凭据的载体

Arcane 侧目前**没有任何**为 `docker_host` 注入凭据的通道（`dockerClientOptionsInternal` 只传 `WithHost` 和 https 的 TLS 配置），且 `NormalizeDockerHost` **显式拒绝 URL 内嵌凭据**。

| 方案 | 做法 | 评价 |
|---|---|---|
| **D2-A（推荐）新增 `docker_host_token` 列** | migration 098 加列；`dockerClientOptionsInternal` 追加 `client.WithHTTPHeaders(map[string]string{"Authorization": "Bearer " + token})` | 与现有 `AccessToken` 对称；可复用现成的加密落库工具（`pkg/utils/update_value_util.go:ApplyEncrypted`、`crypto.Encrypt/Decrypt`、`ContainerRegistryService.GetDecryptedToken` 的写法）；token 不进 URL、不进日志 |
| D2-B URL userinfo | 放宽校验，`tcp://token@nodeget:…` | 密钥出现在 UI/日志/DB 明文；moby 又会丢掉 userinfo，得手工解析——不推荐 |
| D2-C 不走 header，靠网络层 | unix socket 权限 / mTLS | **D1-A 下这就是最优解**，无需新字段 |

**决策（已确认）**：D1-B 在范围内，因此 **采用 D2-A** —— 迁移 098 给 `environments` 加 `docker_host_token TEXT` 列，加密落库，`dockerClientOptionsInternal` 注入 `Authorization: Bearer <token>`。D1-A 形态下凭据可选（文件权限已足够），但 NodeGet 侧仍统一走「端点 + token」的描述符，让 Arcane 侧不必区分形态。

> **缓存键注意**：`hostClients` 目前以 endpoint 字符串为键。token 轮换时必须 `CloseHostClient(host)` 让下一个请求重连，否则会一直用旧凭据。

### D3. 配对方向（谁驱动环境生命周期）

| 方案 | 驱动方 | 评价 |
|---|---|---|
| **D3-A（Phase 1，推荐先做）一次性创建** | 在 Arcane「新建环境」里选 NodeGet，填 URL + token，从下拉里选 agent → 一次 API 调用创建 docker-host 环境 | 改动最小，无同步循环与幂等负担；「一键」体验已达成 |
| **D3-B（Phase 2）Arcane 侧 NodeGet provider** | Arcane 保存 NodeGet URL + token，周期性（或事件驱动）拉 `agent-uuid_list_all`，**自动**为每个 agent 创建/更新/停用环境 | 体验最好、新增 agent 自动纳管；代价是幂等、重命名、删除、token 轮换、防抖等生命周期问题 |
| D3-C NodeGet 推送 | NodeGet 保存 Arcane API key，主动创建环境 | 把 Arcane 的 admin 凭据交给 NodeGet，权限面变差；环境生命周期被两个系统同时改，容易打架 |

**建议**：Phase 1 做 **D3-A**，稳定后再评估 D3-B。**不要**做 D3-C。

### D4. 隧道何时开启

- **默认关闭**：NodeGet server 需显式 `[docker_tunnel] enable = true`；agent 需显式允许该任务类型（沿用 `allow_task_type` 白名单语义，新增 `allow_docker_tunnel`）。
- **按需开启**：仅当存在「启用了隧道」的 agent 时才建隧道，而不是所有 agent 无脑连。
- **幂等**：同一 agent 重复请求只保留一条隧道。

### D5. 是否需要新 TaskEventType

**需要。** 现有 `TaskEventType`（`crates/ng-task/src/types/mod.rs:116-148`）里没有能承载「长期双向字节流」的类型（WebShell 是 PTY 专用，HttpRequest 是短请求，Execute 是子进程）。建议新增 `DockerTunnel`（或更通用的 `ByteTunnel`），**照抄 WebShell 的投递方式**。

新增一个任务类型**至少**要动下面 7 处（已逐处核对；第 5 项是穷尽 match，漏改会直接编译失败）：

| # | 位置 | 要加什么 |
|---|---|---|
| 1 | `crates/ng-task/src/types/mod.rs` | 新枚举变体（:116-148）、`task_name()`（:153-168）、`permission_field()`（:193-207）、新的 `TaskEventResult` 变体（:226-256）及其 `task_name()`、测试（:330/410/523/657/685） |
| 2 | `crates/ng-task/src/rpc/create_task.rs:22` | `validate_task_type` 放行 |
| 3 | `crates/ng-task/src/rpc/query.rs:47-60` | `all_task_types` 数组（注释明确要求与 `task_name()` 同步） |
| 4 | `crates/ng-task/src/rpc/delete.rs:47-60` | 同一个数组（重复了一份） |
| 5 | `agent/src/tasks/mod.rs` | `mod` 声明（:132-144）、`is_pool_managed`（:119）、`is_task_allowed`（:155-177，**穷尽 match 无 `_`**）、`execute_task_direct` 新分支（:224-323） |
| 6 | 新文件 `agent/src/tasks/docker_tunnel.rs` | 任务实现本体（参照 `agent/src/tasks/pty.rs`） |
| 7 | `crates/ng-config/src/config/agent.rs` | `Server` 新增 `allow_docker_tunnel: Option<bool>`（:78-124 内）、手写 `Debug`（:126-151）、示例 TOML 与测试夹具（:363、:529） |

不需要动的：ng-core 的 `Permission::Task(Task::Create(String))` 是**按名字泛化**的（`data_structure.rs:165-176`），ng-token 的匹配是通配符式（`crates/ng-token/src/get.rs:401-452`）。只有当你想新增一个 RPC namespace 时才需要改 `server/src/rpc_nodeget.rs:538-587` 的 `build_modules()`。

**server 侧接入点**：抄 `crates/ng-terminal/src/check_agent.rs:32-71` 的模式 —— agent 拨入时用一次性 `task_token` 反查 task 表、**并校验 task 类型必须是隧道任务**，否则这个端点就成了任意 socket 代理。会话表抄 `crates/ng-terminal/src/terminal.rs:34-69`。

**并发上限**：抄 `WEBSHELL_MAX_SESSIONS`（`agent/src/tasks/mod.rs:108, 441-446`，非阻塞 `try_acquire`），设 `DOCKER_TUNNEL_MAX_SESSIONS`。

---

## 4. 配置结构

### 4.1 NodeGet server

新增 section（示意，表名/字段名待定）：

```toml
[docker_tunnel]
# 总开关，默认 false。Docker socket 等价 host root，必须显式开启。
enable = false

# 监听方式（两种形态可同时开启；也可只开其一）
#   unix socket 目录（D1-A，同机/共享卷）：为每个启用隧道的 agent 生成 <agent_uuid>.sock
socket_dir = "/run/nodeget/docker"
#   TCP 监听（D1-B，跨机）：地址 + 路径前缀
# listen = "127.0.0.1:23750"
# path_prefix = "/agent"

# 访问控制
require_token = true          # TCP 模式必开；unix 模式由文件权限兜底
socket_file_mode = "0600"     # unix socket 文件权限
socket_file_uid = 0           # 属主（或让 Arcane manager 以同组身份读取）
socket_file_gid = 0

# 行为
idle_timeout_secs = 300       # 隧道空闲回收
max_tunnels = 0               # 0 = 不限制（受 agent 数上限约束）
```

要点：

- **默认监听 loopback 或 unix socket**，`enable = false` 时完全不建监听；
- 未启用隧道的 agent 即使收到任务也应拒绝；
- 配置落点：在 `ServerConfig`（`crates/ng-config/src/config/server.rs:13-62`）上加一个 `pub docker_tunnel: Option<DockerTunnelConfig>` 字段，它就自动成为 `[docker_tunnel]` section；
- **监听地址变更不能热生效**。NodeGet 的热重载是「整体拆掉重建」：`notify_one()`（`crates/ng-config/src/server_rpc.rs:191-193`）→ `serve()` 返回 → `server/src/main.rs:98-132` 重新解析配置并重启 runtime。任何与配置绑定的运行时状态（隧道监听、隧道表）都必须显式挂到 `main.rs:128-129` 那一串 reload 重置调用旁边（现有例子：`ng_static::reload_static_path()`、`ng_static::clear_dav_handler_cache()`），否则重载后会**静默保留旧监听**或**丢弃全部隧道**。
- 建议：`enable` / `listen` / `socket_dir` 变更走「重启 runtime」；`require_token`、`idle_timeout_secs`、`max_tunnels` 允许热更新。

### 4.2 NodeGet agent

```toml
[[server]]
# …既有字段…
allow_docker_tunnel = false   # 默认关闭；等价于 allow_task_type 白名单里加 "docker_tunnel"

[docker_tunnel]              # 可选，覆盖默认值
socket_path = "/var/run/docker.sock"
max_sessions = 4
```

落点：`allow_docker_tunnel` 加在 `Server`（`crates/ng-config/src/config/agent.rs:78-124`）里，并按第 3 节 D5 的第 7 项同步手写 `Debug`（:126-151）与示例 TOML/测试夹具（:363、:529）。

**前置条件（必须写进文档与安装脚本提示）**：运行 nodeget-agent 的用户必须能读写 `/var/run/docker.sock`（通常在 `docker` 组内）。默认的原生安装脚本**不保证**这一点。注意 agent 侧的 `tokio` 已经启用了 `net`/`io-util`（`agent/Cargo.toml:20`），所以 `tokio::net::UnixStream` 直接用，**不需要改任何依赖**。

### 4.3 Arcane fork

新增（仅 D1-B/C 需要）：

```go
// Environment（backend/internal/environment/model.go，与既有 DockerHost 并列）
DockerHost      *string `json:"dockerHost" gorm:"column:docker_host" sortable:"true"`
DockerHostToken *string `json:"-" gorm:"column:docker_host_token"` // 新增，加密落库，绝不进 JSON
```

`dockerClientOptionsInternal` 扩展（token 需同时穿过 `detectDockerAPIVersion`，见 §1.2 的调用方说明）：

```go
func dockerClientOptionsInternal(host string, token string) []client.Opt {
	opts := []client.Opt{client.WithHost(host)}
	if strings.HasPrefix(strings.ToLower(strings.TrimSpace(host)), "https://") {
		opts = append(opts, client.WithTLSClientConfig("", "", ""))
	}
	if token != "" {
		opts = append(opts, client.WithHTTPHeaders(map[string]string{
			"Authorization": "Bearer " + token,
		}))
	}
	return opts
}
```

**Phase 2 的完整改动面（已按提交后的代码核对，共 5 处）**：

| # | 文件 | 改动 |
|---|---|---|
| 1 | `backend/resources/migrations/{sqlite,postgres}/098_add_environment_docker_host_token.sql` | `ALTER TABLE environments ADD COLUMN docker_host_token TEXT;`（098 空闲） |
| 2 | `backend/internal/environment/model.go` | `DockerHostToken *string`（`json:"-"`）+ 取值 helper |
| 3 | `backend/internal/docker/service.go` | `dockerClientOptionsInternal(host, token)` 注入 header；`detectDockerAPIVersion`（:188-210）传入 token；`GetHostClient`（:117-152）**缓存键纳入 token**；`CloseHostClient` 保持原签名 |
| 4 | `backend/internal/environment/{dockerhost,service}.go` | `validateEnvironmentTargetInternal` / `applyDockerHostUpdateInternal` 放宽「不能有 token」；token 变更时驱逐缓存 client；用 `ApplyEncrypted`（`backend/pkg/utils/update_value_util.go`）落库 |
| 5 | `types/environment/environment.go` + 前端 | `Create.DockerHostToken` / `Update.DockerHostToken`；环境表单加一个密码输入框 |

连带注意：`NormalizeDockerHost` 目前**拒绝 URL 内嵌凭据**（这是对的，保持不变），若引入 `nodeget://` 之类的自有 scheme 才需要放行。

> **缓存键注意**：`hostClients` 目前以 endpoint 字符串为键。若 token 可轮换，轮换时必须 `CloseHostClient(host)` 让下一个请求重连，否则会一直用旧凭据（见 D2 的同一提醒）。

---

## 5. 状态机

### 5.1 Arcane 环境状态机（沿用现有实现，不新增）

```
                创建 docker-host 环境
                        │
                        ▼
                    pending ────── 健康探测成功（Ping OK） ──────▶ online
                        │                                        │
                        │                                    探测失败
                        │                                        ▼
                        │                                   offline / error
                        │                                        │
                        └──────────── 探测失败（保持 pending）◀───┘

任意状态 ── enabled=false ──▶ 不再探测，不再纳入 docker-host 作业列表
```

- `pending → online` 的自动提升发生在 `updateEnvironmentStatus`（`service.go:1334-1342`，日志在 :1341），条件是 **非 edge + 探测结果 online**；docker-host 环境 `is_edge` 恒为 false，因此**天然自配对**。
- 探测逻辑：`health.Probe` → `probeDockerHostInternal` → `GetHostClient` + `Ping`，10s 超时（`children/health/service.go:46-78, 120-132`）。
- `Probe` 的分派顺序：`ID=="0" && !CustomURL` → 本地 socket；`DockerHost != "" && !CustomURL` → Docker ping；`IsEdge && !CustomURL` → edge 隧道；否则直连 `GET /api/health`。
- 健康任务由 `registerHealthJob` 按 `environmentHealthInterval` 周期调度，docker-host 环境注册在**自己的 scheduler namespace**。

**含义**：NodeGet 侧不需要发明任何「配对握手」。只要端点可达且 ping 得通，Arcane 自己就会把它变成 online。

### 5.2 NodeGet 隧道状态机

```
disabled ──enable=true──▶ waiting_agent ──agent 上报能力──▶ tunnel_up
                              ▲                                  │
                              │                            连接断开/回收
                              └──────────────────────────────────┘
```

| 状态 | 进入条件 | 对 Arcane 的可观测后果 |
|---|---|---|
| `disabled` | `[docker_tunnel] enable = false`，或 agent 未允许该任务 | 端点不存在 / 连接被拒 → Arcane 探测失败 → `offline` |
| `waiting_agent` | 隧道已启用，但该 agent 尚未建立隧道 | TCP 连接被接受但对端无响应 / 直接拒绝 → `offline` |
| `tunnel_up` | agent 隧道就绪，docker socket 已连通 | `Ping` 成功 → `online` |
| `socket_error` | agent 在线但 docker socket 不可读（权限/daemon 未运行） | 建议在隧道握手时先 ping 一次，把**具体原因**回传给 Arcane，而不是让 Arcane 只看到笼统的 offline |

### 5.3 两侧状态的对应关系

| NodeGet | Arcane | 备注 |
|---|---|---|
| agent 掉线（WS 断） | 隧道断 → ping 失败 → `offline` | **自动**，无需额外同步逻辑 |
| agent 在线、socket 不可读 | ping 失败 → `offline` | 需要错误原因透出（见 5.2 最后一行） |
| 隧道 token 被撤销 | 401 → `GetHostClient` 失败 → `offline` | 建议 Arcane 侧把 401 与网络错误区分展示 |
| Arcane 环境 `enabled=false` | 不再探测 | NodeGet 侧可据此回收隧道（可选优化） |

> **设计要点：不要为状态同步新建一条控制通道。** 复用 Arcane 已有的「探测即状态」机制，让隧道可用性自然映射到环境 online/offline。仅当需要**区分失败原因**时才增加少量带外信息。

---

## 6. 权限前置检查

### 6.1 Arcane 侧

| 检查项 | 现有机制 | 位置 |
|---|---|---|
| 调用者能否创建/修改环境 | 环境相关 authz 权限 | `internal/environment/handler.go` + `pkg/authz` |
| 代理请求的每环境授权 | `matcher.Lookup(method, suffix)`，**未映射即拒绝（fail closed）**；`Sudo` 权限集放行 | `middleware/environment.go` |
| docker-host 环境是否享受同样的授权 | **是**：docker-host 走 `next(c)`，由常规路由栈按 path 里的 env id 做 env-scoped 授权 | `middleware/environment.go` 的 docker-host 分支 |
| 端点是否合法 | `NormalizeDockerHost` | `internal/environment/dockerhost.go:37-75` |
| 目标是否真的可达 | `TestConnection` / `probeDockerHostInternal` | `internal/environment/service.go:1289`、`children/health/service.go:120-132` |

**结论**：Arcane 侧**无需新增权限模型**。唯一需要审慎的是「谁能写 `docker_host_token`」——它是 host root 级别的凭据，建议比照现有 `access_token` 的处理方式（写权限 = 修改环境的权限）。

### 6.2 NodeGet 侧

| 检查项 | 要求 | 现有可抄的东西 |
|---|---|---|
| 总开关 | `[docker_tunnel] enable = true`，默认 false | — |
| 调用方鉴权 | NodeGet Token 需具备隧道 scope；沿用 `Token/Scope/Permission/Limit` 模型 | 见下方「注意」 |
| agent 是否被授权承载隧道 | `allow_docker_tunnel` / `allow_task_type` 白名单，**未允许则拒绝任务** | `agent/src/tasks/mod.rs:155-177`（穷尽 match） |
| 隧道拨入端点不能被滥用 | 用一次性 `task_token` 反查 task 表，**并校验 task 类型必须是隧道任务**，否则拒绝（否则该端点会变成任意 socket 代理） | `crates/ng-terminal/src/check_agent.rs:32-71` |
| socket 权限 | agent 运行用户对 `socket_path` 有读写权限；失败时上报明确错误 | — |
| 并发与资源 | `max_sessions` / `max_tunnels` 上限，防止隧道堆积 | `WEBSHELL_MAX_SESSIONS`（`agent/src/tasks/mod.rs:108, 441-446`） |
| 审计 | 建链/断链/鉴权失败写日志（含 agent uuid、远端地址） | — |

> **注意（成本被低估的一点）**：NodeGet **没有任何 axum 鉴权中间件**，所有裸 HTTP handler 都是手工鉴权；并且 `Permission` 枚举里**没有**任何 tunnel/proxy/port-forward 变体，也没有「给某个裸 HTTP 路由授权一个独立 token」的通用机制。
>
> - **D1-A（unix socket）**：鉴权就是文件系统权限，**完全绕开**这个问题，这也是它优于 D1-B 的第二个理由。
> - **D1-B（TCP + 路径前缀）**：需要 (a) 在 `crates/ng-core/src/permission/data_structure.rs:60-91` 新增一个 `Permission` 变体（含 serde/通配符处理），(b) 在 handler 里手工做 Bearer 校验——可参照 `crates/ng-static/src/router.rs:360-514` 的 WebDAV 写法（读 `Authorization`、base64 解、`TokenOrAuth` → `check_token_limit`、失败回 401 + `WWW-Authenticate`），(c) 新增依赖 `hyper`（workspace 目前**没有**声明 hyper / tokio-util）。

### 6.3 一键配对前的 preflight 清单

在 UI 上用一个「测试连接」按钮一次性验证（对应 §7）：

1. NodeGet 地址可达，且 token 有效（`agent-uuid_list_all` 能返回）；
2. 目标 agent 在 `agent-uuid_list_all` 结果里；
3. 该 agent 已启用隧道（`allow_docker_tunnel = true`）且隧道处于 `tunnel_up`；
4. 隧道端点的 `_ping` 返回成功，且返回了 API version；
5. 端点字符串通过 `NormalizeDockerHost`。

任何一项失败都要给出**具体原因**（而不是统一的「连接失败」），这是这个功能能否被用户自行排障的关键。

---

## 7. 一键配对流程（D3-A）

### 7.1 时序

```
用户                Arcane UI（fork）              NodeGet server            nodeget-agent
 │                       │                             │                        │
 │ 新建环境 → 选 NodeGet  │                             │                        │
 │──────────────────────▶│                             │                        │
 │                       │ POST /nodeget/rpc           │                        │
 │ 填 URL + token        │  agent-uuid_list_all        │                        │
 │──────────────────────▶│────────────────────────────▶│                        │
 │                       │◀─── agent 列表 ─────────────│                        │
 │ 下拉选 agent          │                             │                        │
 │──────────────────────▶│                             │                        │
 │                       │ 探测端点（Docker _ping）      │                        │
 │                       │────────────────────────────▶│──隧道──▶              │
 │                       │◀─── 200 OK + API version ───│◀─────────              │
 │                       │                             │                        │
 │                       │ POST /api/environments      │                        │
 │                       │  { dockerHost, ... }        │                        │
 │◀── 环境创建，自动 online ─│                             │                        │
```

### 7.2 API 契约（NodeGet 侧需要新增/复用）

| 用途 | 形式 | 备注 |
|---|---|---|
| 枚举 agent | **复用** `agent-uuid_list_all` / `nodeget-server_list_all_agent_uuid` | 已存在，无需新增 |
| 查询隧道状态 | 新增只读 RPC，如 `docker-tunnel_status`，返回 `[{agent_uuid, state, docker_api_version, error}]` | 供 §6.3 的 preflight 使用；也可合并到上一行 |
| 生成端点字符串 | NodeGet 侧返回 `unix:///run/nodeget/docker/<uuid>.sock` 或 `tcp://host:port/agent/<uuid>` | 由 NodeGet 决定寻址形态，Arcane 不必理解 |
| 隧道凭据 | 若走 D2-A，NodeGet 生成一次性 token 并在响应里返回 | 仅返回一次，Arcane 加密落库 |

**建议**：把「端点字符串 + 凭据 + 状态」合成一个 NodeGet RPC，例如

```
docker-tunnel_describe(agent_uuid) -> {
  docker_host:   "tcp://nodeget.example:23750/agent/8f3c…",
  token:         "…只返回一次…",
  state:         "up",
  api_version:   "1.44",
  socket_ok:     true
}
```

Arcane 侧则只做「拿到 descriptor → 校验 → 建环境」，**不需要理解 NodeGet 的内部寻址**。

### 7.3 失败与幂等

| 场景 | 期望行为 |
|---|---|
| agent 重复上报 | NodeGet 侧幂等，只保留一条隧道 |
| 环境已存在（同名 / 同 agent_uuid） | 幂等更新 `docker_host`，不新建；**以 agent_uuid 做关联键**（建议落一个 `docker_host` 之外的稳定标识，或从端点字符串解析） |
| token 轮换 | NodeGet 侧旧 token 失效 → Arcane 侧 `CloseHostClient(host)` 后重连即报 401；UI 需给出「重新配对」入口 |
| agent 从 NodeGet 消失 | Phase 1 不自动删除环境（避免误删），只让健康探测把它变 `offline`；Phase 2（D3-B）再决定是否自动停用 |
| 端点变更（换端口/换 socket 路径） | 视为 update，`CloseHostClient(old)` + 重建 client |

---

## 8. 安全边界与威胁模型

**核心事实：能访问隧道端点的人，等价于那台主机的 root。**
（Docker socket 可以挂载宿主文件系统、起特权容器。）

因此：

1. **默认关闭**。NodeGet 侧不改任何配置就不存在这个端点；Arcane 侧不建环境就不发请求。
2. **默认不暴露到网络**。优先 unix socket（文件权限）；TCP 模式默认绑 loopback，绑非 loopback 必须显式配置并被文档警告。
3. **逐 agent 授权**。隧道凭据只对一个 agent 有效；撤销单个 agent 不影响其它。
4. **失败即拒绝**。沿用两边的 fail-closed 语义：Arcane 未映射路由默认拒绝；NodeGet 未授权 agent 拒绝建链。
5. **不要在公网裸暴露**。若必须跨公网，放在 TLS 之后（Arcane `https://` + `WithTLSClientConfig` 已有分支），最好限定来源 IP。
6. **审计**。隧道建链/断链/鉴权失败必须留痕（谁、哪个 agent、什么时间、结果）。
7. **Docker API 无鉴权概念**。任何「按 user 限制容器」的想法都不成立——拿到 socket 就是全权。所以不要在隧道里尝试做细粒度容器级鉴权。

---

## 9. 实施计划

### Phase 0：确认设计（本文）

**已确认的决策**：D1 = A + B 都做；D2 = 采用 D2-A（新增 `docker_host_token`）；D3 = Phase 1 用 D3-A（一次性创建），D3-B 留待后续评估。

**基线锁定（2026-10-14）**：Arcane 侧以 fork `main` 的 `cec176c6c`（= `backend/v2.16.0-17`）为基线，它已包含全部 docker-host 能力且与 `upstream/main` 同步。实现期若上游发布新 release，按常规 rebase 即可——我们只在它之上增加 `docker_host_token` 一处特性，冲突面固定在 §4.3 那 5 个文件。

### Phase 1：隧道内核 + 同机（D1-A + D3-A）

先把与 HTTP 无关的那条链路做透。

| 仓库 | 工作 |
|---|---|
| NodeGet | ① 新增隧道任务类型（**7 处改动**，§3 D5）+ `agent/src/tasks/docker_tunnel.rs`（拨出 → 连 `socket_path` → 双向字节泵）；② `[docker_tunnel]` 配置 + 热重载钩子；③ 每 agent 一个 `UnixListener` + 裸字节对拷（D1-A）；④ 拨入鉴权（`task_token` 反查 + task 类型校验）与审计；⑤ 隧道注册表与空闲回收；⑥ `docker-tunnel_describe` RPC |
| Arcane fork | **零代码改动**。docker-host 已提交并稳定（`5e1ca4873`），`unix://` 端点今天就能用；这一阶段只是「手工建一个 `docker_host = unix:///run/nodeget/docker/<uuid>.sock` 的环境」来验证 NodeGet 侧。（可选的体验工作：新建环境向导里加 NodeGet 分支 + preflight，先不做也不影响验收。） |

**这一阶段 Arcane 侧改动极小**，是这套设计最大的优势。注意「零改动」指**不需要写代码**，但**必须运行 fork 的 manager 构建**（`ghcr.io/viogus/manager`，或从 `cec176c6c` 自建）——上游 `v2.16.0` 里完全没有 `docker_host`，无法参与验证。验收标准 = §10 的 NodeGet 集成用例全绿（含 hijack 与长流）。

**实现状态：已落地并通过真实端到端验证（2026-10-14，Rust 侧）。**

| 工作项 | 落点 |
|---|---|
| 任务类型 7 处穷尽改动 | `crates/ng-task/src/types/mod.rs`、`crates/ng-task/src/rpc/{create_task,query,delete,mod}.rs`、`crates/ng-task/src/lib.rs`、`agent/src/tasks/mod.rs`、`crates/ng-config/src/config/agent.rs` |
| 无权限校验的内部派发入口 | `ng_task::create_task_internal(manager, target_uuid, task_type, on_inserted)`（server 侧建隧道时调用；`on_inserted` 保证「先登记 pending 再投递任务」） |
| agent 侧任务 | `agent/src/tasks/docker_tunnel.rs`（`build_tunnel_url` 从自身 `ws_url` 派生 `/docker-tunnel` + query；`TunnelGuard` 名额；`handle_docker_tunnel_url` → 连本地 socket → 拨入 WS → `pump_bytes`） |
| server 侧隧道运行时 | `server/src/docker_tunnel.rs`（每 agent 一个 `UnixListener` + socket 文件权限/属主 + `PENDING_TUNNELS` 注册表 + `oneshot` 交接 + 空闲回收 + `/docker-tunnel` 拨入鉴权 `check_tunnel_agent` + `start`/`stop` 热重载） |
| 接入与热重载 | `server/src/subcommands/serve.rs`（`.merge(crate::docker_tunnel::router())` 在 `.fallback()` 之前；`start(config)`；四处 shutdown/reload 分支调 `stop()`）、`server/src/main.rs`（`#[cfg(unix)]`） |
| 配置与文档 | `[docker_tunnel]`（server + agent）、`allow_docker_tunnel`、`docs/guide/config/{server,agent}.md`、`CONTRIBUTING/crates/ng-config.md` |

与本文档的两处**实现期偏差**（其余按文档执行）：

1. **不引入自研多路复用**：采用「每个本地 Docker 连接派发一次任务 + agent 拨出一条专用 WS」模型（即 WebShell 的既有范式），派生的 socket 句柄通过 `oneshot` 交给 `/docker-tunnel` handler。好处是零新增线协议、鉴权直接复用 `task_token` 反查；代价是每条连接产生一行 task 记录与一次订阅往返。D1-B 可复用同一内核。
2. **D1-A 目前不要求 token**（鉴权 = socket 文件权限 `0600` + 属主/属组），但**拨入 leg 仍然强制 `task_token` + task 类型校验**——这是防止「该端点被当成任意 socket 代理」的关键，§6.2 的要求未放松。
3. ⑥ `docker-tunnel_describe` RPC **尚未实现**：Phase 1 的 socket 由 `[docker_tunnel] agents = [...]` 在启动时建立，够用于本阶段验收（§11 第 5 条「生命周期由谁触发」仍待决策）。

### Phase 2：跨机（D1-B + D2-A）

复用 Phase 1 的隧道内核，只换 server 侧入口。

- NodeGet：新增 `hyper` + `tokio-util` 依赖；单端口 + 路径前缀 + 带 keep-alive 的逐请求剥前缀转发（含 `101` 升级透传）+ Bearer 鉴权（新增隧道 `Permission` 变体，手工校验照 `ng-static/src/router.rs:360-514`）。
- Arcane fork：按 §4.3 的 **5 处改动**做 `docker_host_token`（迁移 098 + model + `dockerClientOptionsInternal`/`detectDockerAPIVersion`/缓存键 + 校验放宽 + types/前端）。
- **实现状态（2026-10-14）**：NodeGet 侧 D1-B 与 Arcane fork 侧 D2-A 均已落地，并通过真实 manager 用 `tcp://…/agent/<uuid>` 闭环验证，见 §10.3。相对本节原文的偏差（不新增 `Permission` 变体、依赖选型、无 `tokio-util`）记在 §10.3 末尾。

### Phase 3（可选）：自动发现（D3-B）

- Arcane 侧 NodeGet provider + 同步循环 + 幂等/重命名/删除策略。

### 顺带建议（独立于本设计）

`docs/dev/extension/index.md` 里那个「Docker 管理器 —— 用 Request 任务请求本地 Docker Unix Socket」的愿望，当前**做不到**：`agent/src/tasks/http_request.rs` 的 `HttpRequestTask.url` 是 `url::Url` + reqwest 默认 TCP 连接器，没有任何 unix socket 支持。给它加 `unix_socket: Option<PathBuf>` + reqwest `ClientBuilder::unix_socket()` 就能解锁，**改动面很小且不需要新增任务类型**。如果只是想「让扩展能碰 docker」，这条路的性价比比本设计高得多，建议单独排一个。

---

## 10. 测试策略

| 层级 | 内容 |
|---|---|
| NodeGet 单元 | 隧道任务序列化/反序列化（含 `task_name()` / `permission_field()` / `TaskEventResult` 三处 match 与 `rpc/query.rs`、`rpc/delete.rs` 两个数组的同步）、`allow_docker_tunnel` 门禁、端点字符串生成、`task_token` 反查与 task 类型校验（**必须覆盖「拿 WebShell 的 token 连隧道端点」这一负例**） |
| NodeGet 集成 | 起 agent + server，隧道建立后从端点发 `GET /_ping`、`/version`、`/containers/json`；**重点补 hijack 用例**（attach/exec 的 `101` 后双向字节流）与**长流用例**（`/events`、`logs?follow=1`）；再补 keep-alive 连接上的连续多请求 |
| NodeGet 配置 | 热重载用例：改 `enable`/`listen` 后隧道表被正确重建或保留（`server/src/main.rs:98-132` 的重启循环） |
| Arcane fork | 复用既有的 `dockerhost_test.go` / `dockerhost_client_cache_test.go` / `system_stats_dockerhost_test.go` 等；补 token 注入与轮换导致 client 重建的用例 |
| 端到端 | 在真实主机上：建隧道 → Arcane 建环境 → 仪表盘出数据 → 启停容器 → 看日志 → `offline`（杀掉 agent）→ 自动恢复 |
| 安全 | 未授权 token、越权 agent_uuid、端点被非授权进程访问（unix socket 权限位）、agent 未允许隧道时的拒绝路径 |

### 10.1 已完成的验证（Phase 1，2026-10-14）

**单元测试**（`cargo test -p ng-task -p ng-config -p nodeget-agent -p nodeget-server`，EXIT=0）：

- agent：`redacted_tunnel_url_hides_task_token`、`build_tunnel_url_replaces_path_and_adds_query`、`build_tunnel_url_rejects_invalid_scheme`、`tunnel_guard_enforces_max_sessions`；
- server：`check_tunnel_agent_rejects_other_task_types`（内存 SQLite + 全量迁移；覆盖「WebShell 任务的 id/uuid/token 全对但类型不符 → 拒绝」这一负例、`docker_tunnel` 任务通过、token 不匹配拒绝、agent_uuid 不匹配拒绝）、`prepare_socket_file_sets_mode`、`router_registers_docker_tunnel_route`；
- ng-task / ng-config：类型与配置的既有用例（任务名、权限字段、序列化往返、`[docker_tunnel]` 解析与默认值）。

**真实端到端**（本机 NodeGet server + agent + 真实 Docker daemon；客户端直接 `DOCKER_HOST=unix://<socket_dir>/<uuid>.sock` 走隧道 socket）：

| 用例 | 覆盖点 | 结果 |
|---|---|---|
| `docker version` | keep-alive 连接上的连续多请求 | 通过（server=29.5.2） |
| `docker run --rm alpine sh -c 'echo …; echo … >&2'` | attach hijack（`101` 后裸字节）+ stdout/stderr 分流 | 通过 |
| `echo INPUT \| docker run --rm -i alpine sh -c 'read line; echo GOT:$line'` | stdin 写入 + **半关闭传递**（§2.3） | 通过（`GOT:INPUT`） |
| `docker run` 间隔输出（`sleep` 循环） | 长流 / 分片到达 | 通过 |
| `docker create` + `docker start -a` | 非 `run` 的 attach 路径 | 通过 |
| `docker exec <running> echo …` | exec hijack | 通过 |

**尚未覆盖**：`logs?follow=1` 的长时间流与热重载用例（改 `[docker_tunnel]` 后 listener 重建）；Arcane 侧 token 注入/轮换用例（属 Phase 2）。

### 10.2 真实 Arcane 纳管验证（Phase 1 收尾，2026-10-14）

Arcane fork **零改动**：用 `go build -tags timetzdata ./cmd`（`GOPROXY=goproxy.cn,direct`）把 fork 的 manager 直接跑在本机（`docker_host` 能力来自 fork 已提交的 7 个提交，`version=2.16.0+cdf.2`），NodeGet 侧为本机 `nodeget-server` + `nodeget-agent`（agent 的 `[docker_tunnel] socket_path` 指向 Colima 的 `/Users/cdf/.colima/default/docker.sock`）：

| 步骤 | 命令 | 结果 |
|---|---|---|
| 建环境 | `POST /api/environments`，body `{"dockerHost":"unix:///Users/cdf/ng-e2e-sockets/2222…2222.sock"}` | `200`，`status=pending`、`isEdge=false` |
| 健康探测 | `POST /api/environments/{id}/test` | `{"status":"online"}`；manager 日志 `promoted pending direct environment to online via reachability check`（探测内容就是 Docker `/_ping`，**穿过隧道**） |
| 出数据 | `GET /api/environments/{id}/{containers,images,networks,volumes}` | `200`，分别 7 / 20 / 3 / 1 条真实数据 |
| 事件流 | manager 的 Docker event stream 连 `host=unix:///…/2222…2222.sock` | 建立成功并持续，直到 agent 被杀才 `stopped … unexpected EOF` |
| 差分 | 杀掉本机 agent → 再 `POST …/test` | `500`（探测失败）；重启 agent → 恢复 `online` |

结论：Phase 1 的目标「Arcane 以 `unix://…/<uuid>.sock` 纳管 nodeget-agent 主机的 Docker」已在**真实 Arcane manager** 上闭环，且 manager 侧不需要任何代码改动。

**本轮新增注意事项**：

- **空闲回收会误杀长流**：Docker 的 `/events`、`logs?follow=1` 在安静主机上可能长时间没有任何字节；若 `idle_timeout_secs` 小于该静默期，隧道会被回收（Arcane 的 event stream 有重连，但会丢一段事件）。docker-host 用途建议 `idle_timeout_secs = 0`（不回收），或按 Arcane 的重连间隔权衡。
- **`pending → online` 只在一次成功探测之后发生**：docker-host 的健康任务跑在环境自己的 scheduler namespace 里（`registerHealthJob` 的 `InProcessTargetContext` 分支），刚建好的环境状态是 `pending`（此时 API 其实已经可用）。一键配对（§7）应显式调一次 `POST /environments/{id}/test` 或等健康任务，避免用户把 `pending` 误判为失败。

### 10.3 跨机纳管验证（Phase 2：D1-B + D2-A，2026-10-14）

NodeGet 侧开启 D1-B（`[docker_tunnel] tcp_listen = "127.0.0.1:23750"`、`path_prefix = "/agent"`、`require_token = true`），Arcane fork 侧实现 D2-A（`docker_host_token` + migration 098），两端合起来用**同一台真实 Docker daemon** 闭环：

| 步骤 | 命令 / 输入 | 结果 |
|---|---|---|
| 迁移 | 复用旧 data 目录启动带 098 的 manager | `Resolved database migration state provider=sqlite currentVersion=97 requiredVersion=98` → `Database migrations completed successfully` |
| 建环境（带 token） | `POST /api/environments`，body `{"name":"nodeget-tcp","dockerHost":"tcp://127.0.0.1:23750/agent/2222…2222","dockerHostToken":"<NodeGet token>","enabled":true}` | `200`，`status=pending`、`isEdge=false`、`apiUrl=""` |
| 健康探测 | `POST /api/environments/{id}/test` | `{"status":"online"}` ——证明 manager 注入的 `Authorization: Bearer …` 穿过 NodeGet 的 D1-B 反代并通过 Bearer 校验（探测内容就是 Docker `/_ping`） |
| 出数据 | `GET /api/environments/{id}/containers`、`…/images` | `200`，7 条容器 / 真实镜像列表，与直连 daemon 一致 |
| 负例：不带 token | 同 `dockerHost`，不传 `dockerHostToken` → `POST …/test` | `500`，`failed to negotiate Docker API version: Error response from daemon: unauthorized` |
| 负例：错 token | `dockerHostToken = "bogus:wrongsecret…"` | 同上 `unauthorized` |

结论：**Arcane 现在可以用 `tcp://<nodeget-server>:23750/agent/<agent_uuid>` 纳管另一台机器上的 Docker**——NodeGet 侧不需要 Arcane 的 agent，Arcane 侧不需要任何 NodeGet 插件；token 由 NodeGet 签发、只在 NodeGet 侧校验，manager 只负责把它带在请求头上。

NodeGet 侧 D1-B 的独立验证（不经 Arcane，客户端直接用 `DOCKER_HOST=tcp://127.0.0.1:23750/agent/2222…2222`）：

- 无 `Authorization` → `401` + `WWW-Authenticate: Bearer`；带正确 token → `200`；越权 scope / 错 token → `401`；
- `curl` 在同一连接上连发 3 个请求：`num_connects` 首次 1、其后 0 ——keep-alive + **逐请求剥前缀**成立（moby 会把 basePath 加到每个请求上）；
- `docker version`、`docker run --rm` 的 stdout+stderr、`docker run -i` 的 stdin + 半关闭、`docker exec` 全部通过。

**实现期偏差（相对 §3 D1-B / §6.2）**：

1. **不新增 `Permission` 变体**：TCP 入口直接复用 `Permission::Task(Task::Create("docker_tunnel"))` + `Scope::AgentUuid(<路径里的 uuid>)`——语义即「处理这条 HTTP 请求就是为该 agent 建立一条 docker_tunnel 隧道」。好处是不必改 `ng-token` 的穷尽 `match` 与 `permission_matches` 通配逻辑，运维可以直接用既有 token CRUD 签发跨机隧道凭据。
2. 依赖选型：`hyper 1.10.1`（workspace 新增）、`hyper-util 0.1.20`、`http-body-util 0.1.3`；**未引入 `tokio-util`**（`copy_bidirectional` 在 `tokio::io` 中）。`101` 升级用 `hyper::upgrade::on()` 在客户端侧与隧道侧各取一次，再 `copy_bidirectional`。
3. 隧道每连接的生命周期：websocket 连接被物化成 `AsyncRead + AsyncWrite` 适配器（`Binary` 帧 = 字节流，`Text` 帧 = 半关闭控制帧），因此 hyper 的 HTTP/1.1 codec 直接跑在这条隧道上，不再另写 framing。

**Arcane fork 侧改动清单（10 改 + 3 新增）**：`types/environment/environment.go`（Create/Update 增加 `dockerHostToken`）、`backend/internal/environment/model.go`（列 + `HasDockerHostToken`）、新增 `backend/internal/environment/dockerhost_token.go`（解密 helper）、`inprocess.go`、`middleware/environment.go`、`bootstrap/router_bootstrap.go`（三处把明文 token 放进请求作用域）、`docker/service.go`（options/缓存键/`GetHostClient`/`PingHost`/`CloseHostClient`/`detectDockerAPIVersion`/`GetClient`）、`environment/children/health/service.go`、`environment/service.go`（加密落库 + 更新钩子 + 轮换驱逐 + 健康探测带 token）、`environment/handler.go`、新增迁移 098（sqlite + postgres）、前端 `new-environment-sheet.svelte` + `types/environment.ts` + `messages/en.json`（docker-host tab 里的密码框，构建产物写入 `backend/frontend/dist`）。回归：`go test ./internal/environment/... ./internal/docker/... ./pkg/libarcane/envcontext/...` 全绿（需把 `dockerhost_client_cache_test.go` 的 `GetHostClient` 调用补上新的 token 参数）。

---

## 11. 开放问题

**已决策（用户确认）**

1. ~~D1 选哪个~~ → **D1-A 与 D1-B 都做**，先 A 后 B（理由见 §3 D1）。
2. ~~D2 是否需要~~ → **采用 D2-A**：新增 `docker_host_token` 列 + migration 098。
3. ~~D3 做到哪一步~~ → **Phase 1 用 D3-A（一次性创建）**；D3-B 自动发现留待 Phase 3 评估。

**2026-10-14 再评估**：Arcane fork 侧 docker-host 已从「未提交工作区」变为「已提交、与上游同步的稳定基线」（`cec176c6c` = `backend/v2.16.0-17`，见 §1.2），
上一轮的三条决策**均不需要修改**：docker-host 的端点语义、校验规则、`dockerClientOptionsInternal` 单一注入点、`pending→online` 自动提升全部与设计一致；
唯一新增的发现是「token 必须同时穿过 `detectDockerAPIVersion`、且缓存键要纳入 token」，已并入 §1.2 / §3 D2 / §4.3。

**2026-10-14 实现期落定（Phase 1 已落地，回答/收窄了下面几条）**：

- **命名**：`DockerTunnel` / `docker_tunnel` / `allow_docker_tunnel`（不用 `ByteTunnel`）——先按期最小暴露面，若将来要复用到别的 unix socket，再把内核参数化。
- **多路复用**：不需要。已用「每条本地连接一个任务 + 一条专用 WS」的模型跑通（见 §9），没有引入 stream-id mux。
- **生命周期触发**：Phase 1 由 `[docker_tunnel] agents = [...]` 在 server 启动时建立（第 5 条待决策仍在，按需建立 + `docker-tunnel_describe` 未实现）。
- **D1-A 的 token**：**不要求**（文件权限 `0600` + 属主/属组已足够），但 agent 拨入 `/docker-tunnel` 的 `task_token` + 类型校验是强制的（第 6 条据此收窄）。
- **依赖**：Phase 1 未新增 `hyper`/`tokio-util`（第 4 条只在 D1-B 需要）。

**仍待决策**

1. **命名**：NodeGet 侧任务叫 `DockerTunnel` 还是更通用的 `ByteTunnel`（后者以后能复用到别的 unix socket 场景，但暴露面更大）。
2. **NodeGet agent 的 docker 权限**：是否在安装脚本里自动把 agent 用户加入 `docker` 组？还是只提示、由用户手动处理？
3. **是否要支持「管理 NodeGet agent 所在主机的 Docker」之外的场景**（例如 NodeGet server 自身的 docker）？
4. **新增依赖**：D1-B 需要 `hyper` + `tokio-util`，是否接受？（若强烈反对，跨机可退回 D1-C —— 每 agent 一个回环端口，纯字节拼接，代价是端口段规划与放行。）
5. **隧道的生命周期由谁触发**：是「Arcane 建环境时 NodeGet 才建隧道」（按需，推荐），还是「agent 一连上就建隧道」（简单，但为没人用的 agent 也开了 host root 通道）？
6. **D1-A 下是否也强制 token**：文件权限已经足够，但统一带 token 能让 Arcane 侧不必区分形态、也让「撤销某个 agent」多一条不依赖文件系统的路径。建议 D1-A 默认不要求 token、但允许开启。
7. **fork 的维护与发行归谁**（本轮新增）：docker-host 这 7 个提交**只存在于 `viogus/arcane`，尚未进入上游**。无论 Phase 1 还是 Phase 2，Arcane 侧都必须由这个 fork 构建镜像（`ghcr.io/viogus/manager`、`/agent`，见 `release.yml:146-147`）。需要明确：是否推动 docker-host 上游化，以便长期只维护「`docker_host_token`」这一层？如果上游化无望，则要接受「跟随上游 release 定期 rebase 这 7 个提交」的固定成本。
8. **部署拓扑（2026-10-14 新增，实测发现）**：路由器（`wrt` = 192.168.2.1，aarch64 OpenWrt 24.10，host 网络）上跑着 `ghcr.io/viogus/arcane:latest`（`version=2.16.0+cdf.2`，即带 docker-host 的 fork 构建）+ `nodeget-agent`；但 agent 连的是 `wss://nodeget-controller.cdf.mba`，**NodeGet server 与 Arcane 不在同一台机器上**。因此在该拓扑下 D1-A（同机 unix socket）用不上，二选一：**(a)** 把 NodeGet server 部署到 Arcane 同机（路由器上交叉编译 aarch64-musl 的 `nodeget-server`，D1-A 直接可用）；**(b)** 做 Phase 2 的 D1-B（NodeGet server 上的 TCP + 路径前缀端点）+ Arcane 侧 `docker_host_token`，让跨机的 Arcane 通过 `tcp://<nodeget-server>:<port>/agent/<uuid>` 纳管。需要用户确认走哪条。

---

## 附录 A：关键代码锚点速查

**Arcane fork**

| 主题 | 位置 |
|---|---|
| 端点校验 / 互斥规则 | `backend/internal/environment/dockerhost.go:37-75, 76-103, 105-177` |
| DB 迁移 | `backend/resources/migrations/{sqlite,postgres}/094_add_environment_docker_host.sql` |
| 请求作用域 | `backend/pkg/libarcane/envcontext/envcontext.go:16-69` |
| 进程内服务判定 | `backend/internal/environment/inprocess.go:19-56` |
| 路由与授权 | `backend/internal/middleware/environment.go`（`EnvironmentTarget`、docker-host 分支） |
| Docker client 选项（**凭据注入点**） | `backend/internal/docker/service.go` `dockerClientOptionsInternal` |
| 每端点 client / event bus | `backend/internal/docker/service.go` `GetHostClient`、`HostEventBus`、`listCoalesced` |
| 健康探测 | `backend/internal/environment/children/health/service.go:46-78, 120-132` |
| 状态提升 | `backend/internal/environment/service.go:1328-1360`（提升判定 :1334-1342） |
| 连接测试 | `backend/internal/environment/service.go:1289-1327` |
| Docker client 缓存/驱逐 | `backend/internal/docker/service.go:117-152`（`GetHostClient`）、`:188-210`（`detectDockerAPIVersion`）、`:172-186`（`CloseHostClient`）；调用点 `environment/service.go:423-424, 507-508` |
| 加密落库工具 | `backend/pkg/utils/update_value_util.go:ApplyEncrypted`、`backend/pkg/utils/.../crypto` |

**NodeGet**

| 主题 | 位置 |
|---|---|
| 扩展模型（无插件系统） | `docs/dev/extension/index.md` |
| axum app 构造 / 路由挂载点 | `server/src/subcommands/serve.rs:164-166`（RPC 服务直接 `call`：:172-182、:223-229、:380-386） |
| 新增裸 HTTP 路由的落点 | `serve.rs` 里在 :467/:522 的 `tokio::select!` 之前再 `.route(...)` 一条 |
| 现有 unix socket 监听（**D1-A 范式**） | `serve.rs:427-448, 1115-1131`；配置 `crates/ng-config/src/config/server.rs:26,29` |
| 任务类型枚举 | `crates/ng-task/src/types/mod.rs:116-148`（`task_name()` :153-168、`permission_field()` :193-207、结果枚举 :226-256） |
| 新任务类型的 7 处改动 | 见 §3 D5（含 `rpc/create_task.rs:22`、`rpc/query.rs:47-60`、`rpc/delete.rs:47-60`、`agent/src/tasks/mod.rs:119/132-144/155-177/224-323`） |
| WebShell 任务定义 | `crates/ng-task/src/types/mod.rs:20, 127` |
| agent 侧任务门禁 / 分发 | `agent/src/tasks/mod.rs:155-177`（门禁）、`:224-323`（dispatch）、`:426` |
| WebShell 隧道实现（**模板**） | `agent/src/tasks/pty.rs`（拨出 `:105-126`、`/auto_gen` 展开 `:504-537`、字节泵 `:220-320, 439-484`） |
| 会话表 / 拨入鉴权（**模板**） | `crates/ng-terminal/src/terminal.rs:34-69, 113-155, 174-305`、`crates/ng-terminal/src/check_agent.rs:32-71` |
| 裸 HTTP 手工鉴权（**模板**） | `crates/ng-static/src/router.rs:360-514`（WebDAV Basic auth） |
| Permission 枚举（**需新增变体**） | `crates/ng-core/src/permission/data_structure.rs:60-91, 165-176, 219-222` |
| TokenOrAuth / 校验入口 | `crates/ng-core/src/permission/token_auth.rs:11-23`、`crates/ng-token/src/get.rs:347-394` |
| 并发上限（**模板**） | `agent/src/tasks/mod.rs:108, 441-446`（`WEBSHELL_MAX_SESSIONS`） |
| agent 配置与白名单 | `crates/ng-config/src/config/agent.rs:78-124`（Debug :126-151、夹具 :363/:529） |
| server 配置 + 热重载 | `crates/ng-config/src/config/server.rs:13-62`；`server/src/main.rs:98-132`；`crates/ng-config/src/server_rpc.rs:191-193` |
| agent 出站连接 | `agent/src/rpc/multi_server.rs:129-142`（`ws_url` 原样使用）、`:608 subscribe_to`、`:564 send_to` |
| agent 清单 RPC | `crates/ng-monitoring/src/rpc/agent_uuid/list_all.rs:24`、`crates/ng-monitoring/src/rpc/nodeget/list_all_agent_uuid.rs:55` |
| HttpRequest 无 unix socket | `agent/src/tasks/http_request.rs` |
| agent 默认原生安装 | `docs/guide/install/install-script.md` |

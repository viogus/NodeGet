---
outline: deep
---

# 架构概述

该项目使用 Rust 作为开发语言，若你需要进行二次开发，请务必熟读该文档。

- `Agent` / `探针端` / `客户端` 等均指代 `nodeget-agent`
- `Server` / `服务端` / `主控` 等均指代 `nodeget-server`
- `调用者` / `前端` / `第三方项目` 等均指代使用 `nodeget-server` 提供的 JSON-RPC API 进行处理、展示、使用的项目

## 总览

本项目分为三个部分:

- nodeget-agent: 监控 Agent
- nodeget-server: 服务端，提供 API
- nodeget-lib: 用于存放公共结构体、以及一些 utils 代码

目前还是传统 Client / Server 架构

## 基本亮点

- 细粒度权限支持，可以通过规范权限 Token 以便于第三方集成
- Powered By Rust，server / agent 性能优秀，系统资源占用低
- 活跃的开发团队
- 前后端分离
- ...

## 通信协议

推荐阅读: <https://wiki.geekdream.com/Specification/json-rpc_2.0.html>

`nodeget-server` 提供了一个 WebSocket JSON-RPC 服务器，并在同端口同样提供 HTTP POST JSON-RPC 服务器，除无法进行长连接外与
WebSocket JSON-RPC 无异。

推荐使用 JSON-RPC 进行二次开发时同时兼容 WebSocket 与 HTTP，并优先使用 WebSocket 通信。

在非 Windows 平台可选启用 Unix Socket 监听（`enable_unix_socket` / `unix_socket_path`），该入口复用与 TCP 完全一致的
Axum 主路由。

## 认证流程

Server 所有 RPC 方法统一通过 `TokenOrAuth` 进行认证。`TokenOrAuth` 支持两种格式：

- `key:secret`：Token 模式，分隔符为冒号 `:`。
- `username|password`：账号密码模式，分隔符为管道符 `|`。

如果凭证字符串同时包含 `:` 与 `|`，将优先按 Token 模式解析（冒号优先级更高）。

Token Secret 与密码均使用 **SHA256("NODEGET" + 原始值)** 进行哈希后存储与比较：先写入 `"NODEGET"` 盐值，再写入原始字符串。Token
验证时会用同样的方式重新计算哈希，并与数据库中存储的哈希做常量时间比较。

系统内置 **Super Token（超级令牌）**，数据库中固定为 `id = 1` 的记录，username 固定为 `root`，`token_limit` 为空数组。持有
Super Token 的调用者可以绕过所有 Limit 权限检查。Super Token 的 key/secret 或 username/password 同样通过常量时间比较（
`ct_eq`）验证，防止时序攻击。

## 错误线格式

所有 JSON-RPC 错误都通过 jsonrpsee 的 `ErrorObject` 返回，结构如下：

```json
{
  "jsonrpc": "2.0",
  "error": {
    "code": <int>,
    "message": "<string>",
    "data": <任意>
  },
  "id": ...
}
```

`NodegetError` 会通过错误码映射到 JSON-RPC 的 `code`（如 101/102/103 等）。但需要注意：除了统一的 `NodegetError`
之外，很多命名空间（task、static、KV、crontab、token、js-worker 等）的处理函数会根据具体场景直接构造 ad-hoc `ErrorObject`，返回自定义的
`code` / `message`。因此实际收到的错误码不一定只在 `NodegetError` 枚举范围内，客户端应同时处理通用错误码和具体业务错误消息。

典型 `code` 含义：

- `101`：解析 / 序列化 / IO 类错误
- `102`：权限拒绝或 Token 校验失败
- `103`：数据库错误
- `104`：Agent 连接错误
- `105`：资源未找到
- `106`：UUID 未找到
- `107`：配置未找到
- `108`：输入校验失败
- `999`：其他未归类错误

## 命名空间约定

NodeGet 使用了自定义 jsonrpsee fork（`infinitefield/jsonrpsee`），其命名空间分隔符为下划线 `_`，而不是常见的点 `.`。例如完整方法名为
`nodeget-server_hello`、`js-worker_create`、`task_create_task`、`token_create`。

所有命名空间在 `server/src/rpc_nodeget.rs` 的 `build_modules()` 中合并为一个统一的 `RpcModule`，并缓存在 `get_modules()`
中。实际注册的命名空间包括：

- `nodeget-server`：服务端专属（hello/version/uuid/read_config/edit_config 等）
- `agent` / `agent-uuid`：Agent 监控与 UUID 管理
- `task`：任务派发与结果
- `token`：Token CRUD 与验证
- `kv`：KV 存储
- `db`：数据库注册表
- `js-worker` / `js-result`：JS Worker CRUD 与执行结果
- `crontab` / `crontab-result`：Cron 任务与结果
- `static-bucket` / `static-bucket-file`：静态桶与文件

定义 RPC trait 时必须使用 `#[rpc(server, namespace = "...")]` + `#[method(name = "...")]` 宏，不要手动注册方法。

### HTTP 路由

Server 在监听端口上同时暴露以下 HTTP 路由：

| 路径                               | 方法        | 说明                                                                                    |
|----------------------------------|-----------|---------------------------------------------------------------------------------------|
| `GET /`                          | GET       | 返回包含 Server UUID 和版本信息的 HTML 页面，可用于快速确认服务是否运行                                         |
| `POST /`                         | POST      | JSON-RPC over HTTP 入口                                                                 |
| `WS /`                           | WebSocket | JSON-RPC over WebSocket 入口                                                            |
| `POST /nodeget/rpc`              | POST      | JSON-RPC over HTTP 入口（推荐新增接入使用）                                                       |
| `WS /nodeget/rpc`                | WebSocket | JSON-RPC over WebSocket 入口（推荐新增接入使用）                                                  |
| `/nodeget/worker-route/{name}/*` | ANY       | JS Worker HTTP 路由入口，详见 [HTTP Route 绑定](/api/js_worker/route.md)                       |
| `/worker-route/{name}/*`         | ANY       | **已废弃**，等价于 `/nodeget/worker-route/*`，保留用于迁移过渡                                        |
| `/nodeget/static/*`              | GET/HEAD  | 静态文件服务入口，详见 [Static Bucket File](/api/static_bucket_file/index.md)                    |
| `/nodeget/static-webdav/*`       | WebDAV    | WebDAV 文件管理服务，支持 mount 为网络盘，详见 [Static Bucket File](/api/static_bucket_file/index.md) |
| `/terminal`                      | WebSocket | Terminal WebSocket 代理，详见 [Terminal](/api/terminal/index.md)                           |
| 其他路径                             | ANY       | **Fallback**: 所有未匹配的路径均转发到 JSON-RPC 服务处理（若启用了 `is_http_root`，则优先走静态服务）                |

Fallback 意味着你可以向任意路径发送 JSON-RPC 请求（如 `POST /api`），Server 都会正常处理。

## 数据库

目前兼容了 SQLite 与 PostgreSQL，请根据需要选择。

- 内部测试或小型（Agent 数目 <= 10）可使用 SQLite，性能问题不明显
- 大量 Agent 务必使用 PostgreSQL，表内压缩、`JSONB` 等特性比 SQLite 更省空间，更高效

Server 启动时会自动执行数据库迁移（`Migrator::up()`），无需手动建表或执行 SQL 脚本。版本升级后首次启动即可完成表结构变更。

使用 SQLite 时，Server 会自动开启 WAL（Write-Ahead Logging）模式（`PRAGMA journal_mode=WAL`），以提升并发读写性能。无需手动配置。

## 注意特点

- 任何功能，均不依赖其他功能
  例如：`上报监控信息` 与 `Task 任务获取` 可以在不同地方实现，或只实现其中一个，不影响使用
- UUID 唯一: 虽然可以用户指定每一个 Server / Agent 的 UUID，但设置为 auto_gen 时会在首次启动时随机生成并持久化到配置文件，只要不刻意改变，UUID
  也不会改变
  整个系统内只有 UUID 作为唯一辨别 ID，不存在 `name` / `id` 等易混淆字段

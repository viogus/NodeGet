# Terminal WebShell 总览

WebShell 是 Task 任务系统下的一个特殊功能，也叫「网页 SSH」/ `Terminal`。

> ## Terminal 不是 JSON-RPC 命名空间
>
> Terminal 没有对应的 JSON-RPC 方法，也不属于任何 JSON-RPC 命名空间。它通过 **HTTP WebSocket 路由**
> 工作，Server exposes 且仅 exposes 一个端点：
>
> ```
> /terminal
> ```
>
> 客户端需直接使用 WebSocket 协议连接 `/terminal?...`，而不是通过 JSON-RPC `method` 字段调用。

## 通信流程

Terminal 的通信流程如下:

1. 通过 `task_create_task` 创建一个 `web_shell` 类型的 Task，其中包含 `terminal_id`、`url` 等字段
2. Agent 接收到任务后，主动通过 WebSocket 连接到 Server 提供的 `/terminal`
3. 用户（网页端）通过 WebSocket 连接到 Server 提供的 `/terminal`
4. Server 在 Agent 与用户之间中继 Binary / Text WebSocket Message，实现双向通信

## 连接 URL

### Agent URL

由 NodeGet Server 提供，Agent 连接的 `/terminal` URL 格式如下:

```
ws(s)://HOST(:PORT)/terminal?agent_uuid={agent_uuid}&task_id={task_id}&task_token={task_token}&terminal_id={terminal_id}
```

参数用于校验对应的 Task。携带 `task_id` 与 `task_token` 的连接会被 Server 识别为 Agent 端。

#### /auto_gen 说明

Agent 支持以 `ws(s)://HOST(:PORT)/auto_gen` 作为 `web_shell` Task 的 `url` 字段值，但这不是 Server 的 URL。

- `/auto_gen` 由 **Agent 端**（`agent/src/tasks/pty.rs`）在本地解析，并根据当前 Agent 配置自动生成 `/terminal` 的完整连接
  URL。
- Server 真实接收并处理的终端端点只有 `/terminal`。

因此：

```
ws(s)://HOST(:PORT)/auto_gen      # Agent 会把它重写为 /terminal?...
ws(s)://HOST(:PORT)/terminal?...  # Server 实际暴露的端点
```

用户也可以显式指定任意其他 WebSocket URL（如第三方监控 Server 提供的地址），此时 Server 仅做 Task 透传，不会强制使用
`/terminal`。

### 用户 URL

由 NodeGet Server 提供的，用户连接的 `/terminal` URL 格式如下:

```
ws(s)://HOST(:PORT)/terminal?agent_uuid={agent_uuid}&terminal_id={terminal_id}&token=demo_token
```

未携带 `task_id` / `task_token` 的连接会被 Server 识别为用户端，此时需要 `token` 用于鉴权。

## terminal_id 说明

`terminal_id` 必须与创建 `web_shell` Task 时提交的 `terminal_id` 一致

同一个 Agent 下，若本地已经存在相同 `terminal_id` 的终端连接，Server 会拒绝该连接并返回错误码 108，避免会话互相覆盖

## 权限要求

Terminal 功能需要 `AgentUuid` 或 `Global` 作用域下的 `Terminal::Connect` 权限

Terminal 连接仅限于 `WebShell` 类型的任务，且该任务必须尚未完成（结果未上传）。非 WebShell 任务或已完成的任务将被拒绝。

## 注意事项

- WebSocket 帧大小限制为 1 MB，消息大小限制为 4 MB。超出限制的帧/消息将被拒绝。
- 实际上，该通道可以传输任意类型的 WebSocket 数据，包括但不限于心跳包、文本类型与 Binary 类型

后续会把该通道拓展使用方向，敬请期待

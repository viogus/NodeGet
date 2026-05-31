# HTTP Route 绑定

当 `js_worker.route_name` 不为 `null` 时，该脚本会启用 HTTP 路由入口。

路由路径（推荐）：

- `http://{host}/nodeget/worker-route/{route_name}`
- `http://{host}/nodeget/worker-route/{route_name}/*`

::: tip 路径迁移说明
旧的 `/worker-route/{route_name}` 与 `/worker-route/{route_name}/*` 仍然可用，行为与新路径**完全一致**，仅为给接入方提供迁移过渡。

**为什么迁移：** NodeGet 内置 HTTP 端点正在统一到 `/nodeget/` 前缀下（当前还有 `/nodeget/static/*`
），可以避免与业务自定义路径冲突，同时也便于反向代理规则统一。

**建议：** 新接入请直接使用 `/nodeget/worker-route/`；已有接入请在方便时将反向代理 / 客户端中的调用地址切换到新前缀。后续版本会移除旧路径。
:::

## 启用方式

在 `js-worker_create` 或 `js-worker_update` 里传入 `route_name`。

约束：

- `route_name` 必须唯一（数据库唯一索引）
- 只允许字符：`a-z A-Z 0-9 . _ -`
- 长度最长 128 字符
- 不能是 `.` / `..` 等纯点组合
- 请求体大小限制为 **8 MB**，超出会返回 HTTP 413 错误

## 脚本入口

开启路由后，脚本需要实现：

```js
export default {
  async onRoute(request, env, ctx) {
    return new Response("ok", { status: 200 });
  }
};
```

- `request` 为 Fetch 标准 `Request` 对象
- 返回值必须是 Fetch 标准 `Response` 对象

## 请求来源 IP（`NG-Connecting-IP`）

`onRoute` 收到的 `request.headers` 中会包含 `NG-Connecting-IP`（头名大小写不敏感）。

- 值为 Server 看到的 TCP 对端 IP 地址。
- 若请求经过本机反向代理，通常会是 `127.0.0.1`（或 `::1`）。
- 若前面是 CDN/边缘节点，通常会是该 CDN/边缘节点的出口 IP。

示例：

```js
const peerIp = request.headers.get("NG-Connecting-IP");
```

## 完整示例

脚本：

```js
export default {
  async onRoute(request, env, ctx) {
    return new Response(
      JSON.stringify({
        ok: true,
        method: request.method,
        path: new URL(request.url).pathname,
        trace_id: randomUUID()
      }),
      {
        status: 200,
        headers: { "content-type": "application/json; charset=utf-8" }
      }
    );
  }
};
```

请求：

```bash
curl -i http://127.0.0.1:2211/nodeget/worker-route/demo_route/hello
```

# Static Bucket File 文件操作

## Upload

上传文件到指定 Bucket 的目录下。

### 方法

调用方法名为 `static-bucket-file_upload`，需要提供以下参数：

```json
{
  "token": "demo_token",
  "name": "my-site",
  "path": "/css/style.css",
  "body": [
    /* 文件二进制内容 */
  ]
}
```

或

```json
{
  "token": "demo_token",
  "name": "my-site",
  "path": "/css/style.css",
  "base64": "Ym9keSB7IGNvbG9yOiByZWQ7IH0="
}
```

### 参数说明

- `name`：bucket 名称
- `path`：文件在 `{static_path}/{path}/` 下的相对路径（如 `/index.html`）
- `body` 与 `base64` **只能二选一**，同时提供会报错
- 文件会自动覆盖原有内容
- 父目录不存在时会自动创建

### 权限要求

- Permission: `StaticBucketFile::Write`
- Scope: `StaticBucket(name)` 或 `Global`

### 返回值

上传成功返回 `{"success": true}`。

### 完整示例

请求:

```json
{
  "jsonrpc": "2.0",
  "method": "static-bucket-file_upload",
  "params": {
    "token": "demo_token",
    "name": "my-site",
    "path": "/index.html",
    "base64": "PCFET0NUWVBFIGh0bWw+PGh0bWw+PGJvZHk+SGVsbG88L2JvZHk+PC9odG1sPg=="
  },
  "id": 1
}
```

响应:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "success": true
  }
}
```

## Read

读取指定 Bucket 目录下的文件内容，以 **base64** 编码返回。

### 方法

调用方法名为 `static-bucket-file_read`，需要提供以下参数：

```json
{
  "token": "demo_token",
  "name": "my-site",
  "path": "/index.html"
}
```

### 权限要求

- Permission: `StaticBucketFile::Read`
- Scope: `StaticBucket(name)` 或 `Global`

### 返回值

返回 base64 编码的字符串。

### 完整示例

请求:

```json
{
  "jsonrpc": "2.0",
  "method": "static-bucket-file_read",
  "params": {
    "token": "demo_token",
    "name": "my-site",
    "path": "/index.html"
  },
  "id": 1
}
```

响应:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": "PCFET0NUWVBFIGh0bWw+..."
}
```

## Delete

删除指定 Bucket 目录下的文件。

### 方法

调用方法名为 `static-bucket-file_delete`，需要提供以下参数：

```json
{
  "token": "demo_token",
  "name": "my-site",
  "path": "/index.html"
}
```

### 权限要求

- Permission: `StaticBucketFile::Delete`
- Scope: `StaticBucket(name)` 或 `Global`

### 返回值

删除成功返回 `{"success": true}`。文件不存在时同样返回成功（幂等）。

### 完整示例

请求:

```json
{
  "jsonrpc": "2.0",
  "method": "static-bucket-file_delete",
  "params": {
    "token": "demo_token",
    "name": "my-site",
    "path": "/index.html"
  },
  "id": 1
}
```

响应:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "success": true
  }
}
```

## Rename

在同一 Bucket 目录内移动/重命名文件。

### 方法

调用方法名为 `static-bucket-file_rename`，需要提供以下参数：

```json
{
  "token": "demo_token",
  "name": "my-site",
  "from": "/index.html",
  "to": "/docs/index.html"
}
```

- `from`：源文件路径，相对 `{static_path}/{path}/`
- `to`：目标文件路径，相对 `{static_path}/{path}/`

### 权限要求

- Permission: 同时需要 `StaticBucketFile::Write` 和 `StaticBucketFile::Delete`
  （rename 语义上等价于"新建 `to`"+"删除 `from`"）
- Scope: `StaticBucket(name)` 或 `Global`

### 行为说明

- 不能跨 bucket 移动：`from` 和 `to` 都在同一 `name` 的磁盘根目录内。
- 源文件不存在 &rarr; 返回 `NotFound` 错误。
- 目标路径已存在时的行为取决于操作系统：`tokio::fs::rename` 在 Unix 上会覆盖目标文件，在 Windows 上会报错（目标已存在）。因此
  **不要依赖跨平台一致的覆盖语义**。
- 自动创建 `to` 缺失的父目录。
- `from == to` 视作 no-op，直接返回成功。
- 路径经 `resolve_safe_file_path` 双重校验，拒绝 `..` 穿透、绝对路径、反斜杠、Windows 盘符等。

### 返回值

成功返回 `{"success": true}`。

### 完整示例

请求:

```json
{
  "jsonrpc": "2.0",
  "method": "static-bucket-file_rename",
  "params": {
    "token": "demo_token",
    "name": "my-site",
    "from": "/old-name.html",
    "to": "/pages/new-name.html"
  },
  "id": 1
}
```

响应:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "success": true
  }
}
```

## List

列出某个 Bucket 目录下的所有文件，含体积和修改时间。

### 方法

调用方法名为 `static-bucket-file_list`，需要提供以下参数：

```json
{
  "token": "demo_token",
  "name": "my-site"
}
```

### 权限要求

- Permission: `StaticBucketFile::List`
- Scope: `StaticBucket(name)` 或 `Global`

### 返回值

返回该 Bucket 目录下所有文件的元信息数组，**按 `path` 字典序排序**，路径统一使用 `/` 作为分隔符（跨平台一致）：

```json
[
  {
    "path": "404.html",
    "size": 1024,
    "mtime": 1715000000000
  },
  {
    "path": "docs/1.md",
    "size": 3,
    "mtime": 1715000123456
  },
  {
    "path": "index.html",
    "size": 7,
    "mtime": 1715001000789
  }
]
```

字段说明：

- `path`：相对 `{static_path}/{path}/` 的路径，`/` 分隔
- `size`：文件大小，单位**字节**（`u64`）
- `mtime`：最后修改时间，**Unix 毫秒时间戳**（`i64`）；若底层文件系统不支持或读取失败，返回 `0`

注意事项：

- 只列出**文件**，不包括目录本身
- 软链接不会被跟随，避免越权访问 `{static_path}/{path}/` 外部内容
- 磁盘目录不存在（例如刚 `static-bucket_create` 但还没上传）时返回空数组 `[]`
- 路径中包含非 UTF-8 分段的文件会被跳过（仅日志告警）

### 完整示例

请求:

```json
{
  "jsonrpc": "2.0",
  "method": "static-bucket-file_list",
  "params": {
    "token": "demo_token",
    "name": "my-site"
  },
  "id": 1
}
```

响应:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": [
    {
      "path": "404.html",
      "size": 1024,
      "mtime": 1715000000000
    },
    {
      "path": "docs/1.md",
      "size": 3,
      "mtime": 1715000123456
    },
    {
      "path": "index.html",
      "size": 7,
      "mtime": 1715001000789
    }
  ]
}
```

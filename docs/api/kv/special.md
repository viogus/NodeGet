# 特殊 Kv 与特殊键

**本页面所有的 Namespace / K-V 都不会由 server 主动创建**

这里列出了一些 Kv 系统中的特殊 Namespace / 键名，是约定俗成的规范

## 特殊 Kv

- 每一个 Agent 的以自身 Uuid 为 Name 的 Namespace，称之为 `Agent Namespace`

- 每一个 Server 中以 `global` 为 Name 的 Namespace，称之为 `Server Namespace` 或直接称为 `global`

- 每一个 Server 中以 `frontend_[主题名]` 为 Name 的 Namespace，称之为 `Frontend Namespace`，是用于存放给主题所需要的配置文件的，比如官方的前端为
  `frontend_nodeget`

- 每一个 Token 的以自身 Token Key 为 Name 的 Namespace，称之为 `Token Namespace`

## 特殊键

在一个 Kv 中，非 Agent / Server 开发者不建议使用以下的键，其在 Agent / Server 内部有特殊用途，或为共同认定的功能键

### 所有 Namespace 通用

- `__nodeget_namespace_marker__`: Namespace 占位符。创建 Namespace（`kv_create`）时自动写入，value 为 `null`。Server 通过该
  key 是否存在来判断 Namespace 是否已创建。该 key 公开可见，可通过 `kv_get_all_keys`、`kv_get_value` 读取，也可通过
  `kv_set_value` 修改其 value。若通过 `kv_delete_key` 删除该 key 且 Namespace 下无其他 key，则该 Namespace 将被视为不存在。

### Agent Namespace

- `database_limit_*`:
    - `database_limit_static_monitoring`: 单位毫秒。数据清理任务在 Static 表中查询最后一个该
      Uuid 的数据，获取其 Timestamp。清理 `从 (Timestamp - 该值) 至 Timestamp` **以外的**
      数据，可以理解为清理旧数据，保留新数据。该设置不受数据条数影响，仅以 Timestamp 为标准。若某一 Agent
      设置了该值，并在历史某一时刻不再上传数据，则不会影响其 `从 (最后一个 Timestamp - 该值) 至 最后一个 Timestamp` 的数据
    - `database_limit_dynamic_monitoring`: 同上，Dynamic Monitoring Data
    - `database_limit_dynamic_monitoring_summary`: 同上，Dynamic Monitoring Summary Data
    - `database_limit_task`: 同上，Task 记录
- `metadata_*`:
    - `metadata_name`: 前端展示 Agent 名字
    - `metadata_tags`: 前端展示的 Tag，为数组，值为 String，如:

      ```json
      ["tag1", "tag2"]
      ```

    - `metadata_price`: 前端展示的机器价格
    - `metadata_price_unit`: 前端展示的机器单位
    - `metadata_price_cycle`: 前端展示的续费周期，单位天
    - `metadata_expire_time`: 续费到期时间，单位毫秒时间戳（常用于续费提醒相关扩展 / 主题）
    - `metadata_region`: 前端展示的地区代码，遵循 ISO 3166-1 二位字母代码（仅作为展示，不代表 IP 地址所在地区）
    - `metadata_longitude` `metadata_latitude`: 经纬度
    - `metadata_hidden`: 前端中隐藏，不代表没有权限访问

### 保留期下限（防止误清空）

`database_limit_task` / `database_limit_crontab_result` / `database_limit_js_result`
供服务端保留期清理器（`[retention] enabled = true` 时真正删除）读取。为避免把「天」误填成
毫秒、或填 `0` 导致整表被清空，低于下限的值会被**忽略并打 warn**：

| 键 | 下限 |
| --- | --- |
| `database_limit_task` | 10 分钟（600000） |
| `database_limit_crontab_result` | 1 小时（3600000） |
| `database_limit_js_result` | 1 小时（3600000） |

Agent 级的非法值会回退到 `global` 级；两级都没有则不清理该 Agent（保守策略）。

### 保留命名空间

以 `__` 开头的命名空间由服务端内部使用（如审计数据 `__js_audit`、审计开关 `__js_policy`），
`kv_create` / `kv_set_value` / `kv_delete_key` / `kv_delete_namespace` 一律返回 `102` 拒绝写入；
读取不受限制，方便排查。

### Server Namespace

- `database_limit_crontab_result`: 与 `database_limit_*` 类似，Crontab 执行记录，但必须存在于 `global` Kv 中，其他位置无效
- `database_limit_js_result`: 同上，JS Worker 执行记录（时间基准 `start_time`），同样必须存在于 `global` Kv 中。
  服务端保留期清理器会读取它；未配置时该表不清理
- `database_limit_traffic_snapshot`: 单位毫秒。流量统计快照的保留时长，同样必须存在于 `global` Kv 中，其他位置无效。由流量统计 Worker（
  `traffic-snapshot-worker`）读取，每小时删除比 `现在 - 该值` 更早的快照以及整段早于该时间的"可能丢失数据"记录。没有配置时默认 365 天；小于 1
  小时或不是整数的值视为不合法，同样回退为默认值（避免把"天数"误填成毫秒而清空所有快照）
- `traffic_snapshot_interval`: 单位毫秒。流量统计快照的间隔，必须存在于 `global` Kv 中，其他位置无效。由流量统计 Worker 读取，每个时间段（按
  UTC 时间对齐）最多存一条快照。必须是 60000（1 分钟）的整数倍，没有配置时默认 900000（15 分钟），不合法的值回退为默认值。建议选能整除一天的值（如
  5、10、15、20、30、60 分钟），不能整除一天的值也能用，只是每天的整点时刻会漂移

上面两个键由 Bootstrap 里的流量统计 Worker 使用，Server 本身不读取，详见[流量统计](/guide/features/traffic)。

### Frontend Namespace

- `title`
- `description`
- `custom_body`
- `custom_head`

### Token Namespace

- `frontend_custom_body`
- `frontend_custom_head`

需要解释的是，Frontend 和 Token 的 Namespace 并不冲突。

Token Namespace 意为在使用这一 Token 登陆后，前端会使用的参数（这里的 Token 应该是供给展示的），前端可以进行拼接或优先使用
Token Namespace 定义的参数

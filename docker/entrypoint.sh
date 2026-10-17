#!/bin/sh
set -eu

# 配置文件路径，按优先级：
#   1. NODEGET_CONFIG 显式指定
#   2. /etc/nodeget/config.toml（历史镜像/挂载约定）
#   3. /nodeget/config.toml（本镜像默认，可写层）
# 已存在的配置一律沿用，不再根据环境变量重写——避免「重启把运维手改的配置覆盖掉」。
CONFIG="${NODEGET_CONFIG:-}"
if [ -z "${CONFIG}" ]; then
    if [ -f /etc/nodeget/config.toml ]; then
        CONFIG=/etc/nodeget/config.toml
    else
        CONFIG=/nodeget/config.toml
    fi
fi

# 数值环境变量校验：这些值会被直接写进 TOML，非法值（如 "30d"、空串）会在启动时
# 变成一个难懂的解析错误，这里提前给出明确信息；同时避免把任意字符串注入配置文件。
require_uint() {
    case "${2}" in
        '' | *[!0-9]*)
            echo "[nodeget] ${1} must be a non-negative integer, got '${2}'" >&2
            exit 1
            ;;
    esac
}

if [ ! -f "${CONFIG}" ]; then
    db_url="${NODEGET_DATABASE_URL:-sqlite:///nodeget/nodeget.db?mode=rwc}"
    cat > "${CONFIG}" <<EOF
ws_listener = "0.0.0.0:2211"
server_uuid = "auto_gen"

[logging]
log_filter = "info"

[database]
database_url = "${db_url}"
EOF

    # TimescaleDB 时序优化（可选）：设置任一 NODEGET_TIMESCALE_* 环境变量即启用。
    # 仅当数据库是安装了 timescaledb 扩展的 PostgreSQL 时生效。
    # 小时键优先于天键（天键为历史配置保留）；retention_days 默认 0 = 不启用自动删除。
    if [ -n "${NODEGET_TIMESCALE_CHUNK_INTERVAL_DAYS:-}" ] \
        || [ -n "${NODEGET_TIMESCALE_COMPRESS_AFTER_DAYS:-}" ] \
        || [ -n "${NODEGET_TIMESCALE_RETENTION_DAYS:-}" ] \
        || [ -n "${NODEGET_TIMESCALE_CHUNK_INTERVAL_HOURS:-}" ] \
        || [ -n "${NODEGET_TIMESCALE_COMPRESS_AFTER_HOURS:-}" ]; then
        chunk_days="${NODEGET_TIMESCALE_CHUNK_INTERVAL_DAYS:-1}"
        compress_days="${NODEGET_TIMESCALE_COMPRESS_AFTER_DAYS:-7}"
        retention_days="${NODEGET_TIMESCALE_RETENTION_DAYS:-0}"
        require_uint NODEGET_TIMESCALE_CHUNK_INTERVAL_DAYS "${chunk_days}"
        require_uint NODEGET_TIMESCALE_COMPRESS_AFTER_DAYS "${compress_days}"
        require_uint NODEGET_TIMESCALE_RETENTION_DAYS "${retention_days}"

        cat >> "${CONFIG}" <<EOF

[database.timescale]
chunk_interval_days = ${chunk_days}
compress_after_days = ${compress_days}
retention_days = ${retention_days}
EOF

        # 小时级键（新配置优先于上面的天级键）
        if [ -n "${NODEGET_TIMESCALE_CHUNK_INTERVAL_HOURS:-}" ]; then
            require_uint NODEGET_TIMESCALE_CHUNK_INTERVAL_HOURS "${NODEGET_TIMESCALE_CHUNK_INTERVAL_HOURS}"
            echo "chunk_interval_hours = ${NODEGET_TIMESCALE_CHUNK_INTERVAL_HOURS}" >> "${CONFIG}"
        fi
        if [ -n "${NODEGET_TIMESCALE_COMPRESS_AFTER_HOURS:-}" ]; then
            require_uint NODEGET_TIMESCALE_COMPRESS_AFTER_HOURS "${NODEGET_TIMESCALE_COMPRESS_AFTER_HOURS}"
            echo "compress_after_hours = ${NODEGET_TIMESCALE_COMPRESS_AFTER_HOURS}" >> "${CONFIG}"
        fi
    fi

    # 数据库保留期清理（fork 功能）：默认 dry-run，必须显式打开才会真删。
    # 保留期本身仍来自 kv 的 database_limit_*，这里只控制「清理器是否执行删除」。
    retention_enabled="${NODEGET_RETENTION_ENABLED:-false}"
    case "${retention_enabled}" in
        true | false) ;;
        *)
            echo "[nodeget] NODEGET_RETENTION_ENABLED must be 'true' or 'false', got '${retention_enabled}'" >&2
            exit 1
            ;;
    esac
    {
        echo ""
        echo "[retention]"
        echo "enabled = ${retention_enabled}"
        if [ -n "${NODEGET_RETENTION_INTERVAL_SECS:-}" ]; then
            require_uint NODEGET_RETENTION_INTERVAL_SECS "${NODEGET_RETENTION_INTERVAL_SECS}"
            echo "interval_secs = ${NODEGET_RETENTION_INTERVAL_SECS}"
        fi
    } >> "${CONFIG}"
fi

exec nodeget-server serve -c "${CONFIG}"

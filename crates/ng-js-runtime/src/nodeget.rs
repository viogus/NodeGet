//! `nodeget()` API —— JS 上下文内部发起 JSON-RPC 调用。
//!
//! JS 端通过 `globalThis.__nodeget_rpc_raw(json)` 触发此模块，
//! 将请求转发到服务器 `RpcModule` 进行分发。支持单条和批量请求。

use crate::js_worker_service::get_js_worker_service;
use crate::server_runtime::js_error;
use crate::spawn_on_server_runtime::spawn_on_server_runtime;
use rquickjs::Error;
use rquickjs::function::Opt;
use serde_json::value::RawValue;
use std::result::Result as StdResult;
use tracing::{debug, trace};

/// RPC 响应缓冲区大小。
/// 足以容纳绝大多数 JSON-RPC 响应，避免因缓冲区过小导致截断重试。
const RPC_BUF_SIZE: usize = 4096;

/// 从 JS 上下文发起 `nodeget()` RPC 调用，返回响应 JSON 字符串。
///
/// - `json` —— JSON-RPC 请求字符串（单条或批量数组）
/// - `worker` —— 发起调用的 Worker 名（由 JS 包装器传入，仅用于审计）。用
///   `Opt<Option<String>>` 以同时容忍三种调用形态：参数缺失、传 `null`/`undefined`
///   （记为 `<unknown>`，例如脚本绕过包装器直接调用 `__nodeget_rpc_raw`）或传名字。
///
/// 内部步骤：
/// 1. 判断是否为批量请求（以 `[` 开头）
/// 2. 批量请求：单次获取 RPC module 后分发所有子请求，避免每条子请求独立 spawn
/// 3. 单条请求：直接通过 `spawn_on_server_runtime` 执行
/// 4. 分发前对每个子请求做一次审计记录（观察模式，不拦截）
///
/// # Errors
/// 若 JSON-RPC 请求执行失败，返回 `rquickjs::Error`。
pub async fn js_nodeget(json: String, worker: Opt<Option<String>>) -> StdResult<String, Error> {
    let worker = worker.0.flatten();
    debug!(target: "js_runtime", "handling JS nodeget RPC call");
    let trimmed = json.trim();

    // 审计 flush 循环懒启动（幂等）；审计本身不参与裁决
    crate::js_audit::ensure_flusher();

    // 批量请求：JSON 数组形式，单次获取 RPC module 后依次分发
    if trimmed.starts_with('[') {
        // 使用 RawValue 避免 parse→serialize→parse 的往返开销
        let items: Vec<Box<RawValue>> =
            serde_json::from_str(trimmed).map_err(|e| js_error("jsonrpc_parse", e.to_string()))?;

        for item in &items {
            crate::js_audit::record(worker.as_deref(), method_of(item.get()));
        }

        let items_len = items.len();
        let results = spawn_on_server_runtime(async move {
            let service = get_js_worker_service()
                .ok_or_else(|| "JsWorkerService not initialized".to_string())?;
            let rpc_module = service.get_rpc_module().await;
            let mut results = Vec::with_capacity(items_len);
            for item in &items {
                let req_str = item.get();
                let (resp, _stream) = rpc_module
                    .raw_json_request(req_str, RPC_BUF_SIZE)
                    .await
                    .map_err(|e| e.to_string())?;
                results.push(Ok::<_, String>(resp));
            }
            Ok::<_, String>(results)
        })
        .await
        .map_err(|e| js_error("jsonrpc_module", e))?
        .map_err(|e| js_error("jsonrpc_module", e))?;

        let mut responses = Vec::with_capacity(results.len());
        for result in results {
            responses.push(result.map_err(|e| js_error("jsonrpc_module", e))?);
        }

        Ok(format!("[{}]", responses.join(",")))
    } else {
        // 单条请求
        trace!(target: "js_runtime", "processing raw JSON-RPC request from JS");
        // 若 trim 未改变内容，直接复用原始 String，避免分配
        let json = if trimmed.len() == json.len() {
            json
        } else {
            trimmed.to_owned()
        };

        crate::js_audit::record(worker.as_deref(), method_of(&json));

        let response = spawn_on_server_runtime(async move {
            let service = get_js_worker_service()
                .ok_or_else(|| "JsWorkerService not initialized".to_string())?;
            let rpc_module = service.get_rpc_module().await;
            let (resp, _stream) = rpc_module
                .raw_json_request(&json, RPC_BUF_SIZE)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(resp)
        })
        .await
        .map_err(|e| js_error("jsonrpc_module", e))?
        .map_err(|e| js_error("jsonrpc_module", e))?;

        Ok(response)
    }
}

/// 只解析 JSON-RPC 请求的 `method` 字段，供审计使用。
#[derive(serde::Deserialize)]
struct MethodOnly<'a> {
    #[serde(borrow)]
    method: &'a str,
}

/// 提取请求的 `method`；任何解析失败都返回占位符，绝不影响正常分发。
fn method_of(request: &str) -> &str {
    serde_json::from_str::<MethodOnly<'_>>(request).map_or("<unparsed>", |parsed| parsed.method)
}

#[cfg(test)]
mod tests {
    use super::method_of;
    use rquickjs::function::{FromParam, Opt};

    #[test]
    fn method_of_extracts_rpc_method() {
        let request =
            r#"{"jsonrpc":"2.0","method":"task_create_task_blocking","params":{},"id":"1"}"#;
        assert_eq!(method_of(request), "task_create_task_blocking");
        // 解析失败只返回占位符，不影响分发
        assert_eq!(method_of("not json"), "<unparsed>");
        assert_eq!(method_of("{}"), "<unparsed>");
    }

    #[test]
    fn worker_argument_stays_optional() {
        // 绑定签名是 (String, Opt<Option<String>>)：1 个必填 + 1 个可选。
        // 若把第二个参数写成 Option<String>，rquickjs 会按「必填」计算（min = 2），
        // 只传 1 个参数的 __nodeget_rpc_raw 调用就会报 MissingArgs。
        let json_req = <String as FromParam<'static>>::param_requirement();
        let worker_req = <Opt<Option<String>> as FromParam<'static>>::param_requirement();
        let combined = json_req.combine(worker_req);

        assert_eq!(combined.min(), 1, "worker 名必须是可选参数");
        assert_eq!(combined.max(), 2);
    }
}

//! `js-worker_run` RPC —— 执行 JS Worker。
//!
//! 根据编译模式选择字节码路径（运行时池）或源码路径（一次性 Runtime）。

use crate::js_worker::auth::check_js_worker_permission;
use crate::service::{enqueue_defined_js_worker_run, enqueue_source_js_worker_run};
use jsonrpsee::core::RpcResult;
use ng_core::error::NodegetError;
use ng_core::permission::data_structure::JsWorker as JsWorkerPermission;
use ng_js_runtime::{CompileMode, RunType};
use serde_json::Value;
use serde_json::value::RawValue;
use tracing::debug;

/// 执行指定的 JS Worker。
///
/// - `token` —— 认证 Token
/// - `js_script_name` —— Worker 名称
/// - `run_type` —— 运行模式（默认 Call）
/// - `params` —— 调用参数
/// - `env` —— 环境变量覆盖（可选）
/// - `compile_mode` —— 编译模式（默认 Bytecode）
///
/// 返回 `js_result` 行的 ID，结果异步写入数据库。
pub async fn run(
    token: String,
    js_script_name: String,
    run_type: Option<RunType>,
    params: Value,
    env: Option<Value>,
    compile_mode: Option<CompileMode>,
) -> RpcResult<Box<RawValue>> {
    let process_logic = async {
        let run_type = run_type.unwrap_or(RunType::Call);
        let compile_mode = compile_mode.unwrap_or(CompileMode::Bytecode);

        let script_name = js_script_name.trim().to_owned();

        if script_name.is_empty() {
            return Err(
                NodegetError::InvalidInput("js_script_name cannot be empty".to_owned()).into(),
            );
        }

        debug!(target: "js_worker", script_name = %script_name, run_type = ?run_type, compile_mode = ?compile_mode, "processing js_worker run request");

        check_js_worker_permission(
            &token,
            script_name.as_str(),
            JsWorkerPermission::RunDefinedJsWorker,
        )
        .await?;

        debug!(target: "js_worker", script_name = %script_name, "js_worker run permission check passed");

        let js_result_id = match compile_mode {
            CompileMode::Bytecode => {
                enqueue_defined_js_worker_run(script_name, run_type, params, env).await?
            }
            CompileMode::Source => {
                enqueue_source_js_worker_run(script_name, run_type, params, env).await?
            }
        };

        debug!(target: "js_worker", js_result_id, "js_worker run enqueued successfully");

        let json_str = serde_json::to_string(&serde_json::json!({ "id": js_result_id }))
            .map_err(|e| NodegetError::SerializationError(e.to_string()))?;
        RawValue::from_string(json_str)
            .map_err(|e| NodegetError::SerializationError(e.to_string()).into())
    };

    match process_logic.await {
        Ok(result) => Ok(result),
        Err(e) => {
            let nodeget_err = ng_core::error::anyhow_to_nodeget_error(&e);
            Err(jsonrpsee::types::ErrorObject::owned(
                nodeget_err.error_code() as i32,
                format!("{nodeget_err}"),
                None::<()>,
            ))
        }
    }
}

//! RPC 方法派发 trait。
//!
//! [`RpcDispatcher`] 对 RPC 框架做抽象，允许模块合并而不耦合具体实现。

/// RPC 方法派发 trait。
///
/// 具体实现（如包装 jsonrpsee 的 `RpcModule`）提供框架无关的模块组装能力。
pub trait RpcDispatcher: Send + Sync + Sized {
    /// 将另一个 Dispatcher 合并到当前实例。
    ///
    /// 合并后，`other` 中的所有方法可通过 `self` 访问。
    fn merge(&mut self, other: Self) -> anyhow::Result<()>;
}

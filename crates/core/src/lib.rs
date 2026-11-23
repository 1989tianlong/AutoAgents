//! AutoAgents 核心库：桌面应用（控制端）与 `agentd`（节点）共用的能力层，不依赖 Tauri。
//!
//! - [`adapters`]：把各家 CLI Agent 的无头模式归一成统一的命令与事件
//! - [`runner`]：执行一轮对话（拉起进程 → 解析输出 → 推送事件 → 收尾）
//! - [`service`]：机器侧服务，控制端对「本机」和「远程节点」发的是同一套请求
//! - [`discovery`] / [`models`] / [`machine`]：本机 Agent 发现、模型清单、主机指标
//!
//! 控制端把本机也当作一个节点（[`service::Service`] 在应用进程内直接调用），
//! 远程机器则由 agentd 运行同一个 [`service::Service`]，请求经加密通道转发过去。

pub mod adapters;
pub mod discovery;
pub mod machine;
pub mod models;
pub mod runner;
pub mod service;

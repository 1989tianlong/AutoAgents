//! 测试共用的小工具：冒充 Agent 的脚本、收集推给界面的事件、组装测试用的应用状态。

use crate::chat::{ChatEvent, Sink};
use crate::remote::{NodeSink, NodeStatusEvent};
use crate::secrets::MemoryStore;
use crate::AppState;
use autoagents_core::runner::BinaryResolver;
use autoagents_core::service::{Service, ServiceOptions};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 收集推给界面的事件
#[derive(Clone)]
pub struct Collect<T>(pub Arc<Mutex<Vec<T>>>);

impl<T> Default for Collect<T> {
    fn default() -> Self {
        Self(Arc::default())
    }
}

impl Sink for Collect<ChatEvent> {
    fn send(&self, event: ChatEvent) {
        self.0.lock().unwrap().push(event);
    }
}

impl NodeSink for Collect<NodeStatusEvent> {
    fn send(&self, event: NodeStatusEvent) {
        self.0.lock().unwrap().push(event);
    }
}

/// 写一个冒充某个 Agent 的脚本，返回它的路径
pub fn fake_script(agent: &str, script: &str) -> PathBuf {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join(agent);
    std::fs::write(&bin, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    std::mem::forget(dir);
    bin
}

/// 只认 `bin` 的机器侧服务；`name` 是它对外报的机器名
pub fn service_for(bin: PathBuf, name: Option<&str>) -> Arc<Service> {
    Service::new(ServiceOptions {
        name: name.map(str::to_string),
        resolve: Some(Arc::new(move |_: &str| Some(bin.clone())) as BinaryResolver),
        ..Default::default()
    })
}

/// 造一个冒充 claude 的脚本，并返回它的路径和只认它的服务
pub fn fake_agent(script: &str) -> (PathBuf, Arc<Service>) {
    let bin = fake_script("claude", script);
    let service = service_for(bin.clone(), None);
    (bin, service)
}

/// 测试用的应用状态，连同它收事件的两个收集器
pub struct TestState {
    pub state: Arc<AppState>,
    pub secrets: Arc<MemoryStore>,
    pub chat: Collect<ChatEvent>,
    pub nodes: Collect<NodeStatusEvent>,
}

/// 组装一份测试用的应用状态：内存数据库、内存凭据库
pub fn test_state(service: Arc<Service>) -> TestState {
    let chat = Collect::default();
    let nodes = Collect::default();
    let secrets = Arc::new(MemoryStore::default());
    let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
    let state = AppState::new(
        db,
        service,
        Arc::new(chat.clone()),
        Arc::new(nodes.clone()),
        secrets.clone(),
    );
    TestState {
        state,
        secrets,
        chat,
        nodes,
    }
}

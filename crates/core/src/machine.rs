//! 主机信息与资源指标：机器页的卡片、仪表盘的迷你图都从这里取。

use crate::discovery;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};
use sysinfo::{
    CpuRefreshKind, LoadAvg, MemoryRefreshKind, ProcessRefreshKind, RefreshKind, Signal, System,
    UpdateKind,
};

/// CPU 占用率要取两次采样之差，间隔太短读数没有意义
const CPU_SAMPLE: Duration = Duration::from_millis(300);

/// 一台机器的静态信息
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineInfo {
    /// 显示名（节点自己配置的名字，没有则用主机名）
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub kernel: Option<String>,
    /// 提供服务的程序版本：桌面应用内置，或 agentd
    pub version: String,
    pub flavor: String,
    pub home: String,
    pub uptime_s: u64,
    pub cpu_count: usize,
    pub mem_total: u64,
}

/// 一台机器此刻的资源占用
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    /// 全核平均 CPU 占用率（%）
    pub cpu_percent: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    /// 1 / 5 / 15 分钟平均负载（Windows 上恒为 0）
    pub load1: f32,
    pub load5: f32,
    pub load15: f32,
    pub uptime_s: u64,
    /// 这台机器上正在运行的对话轮数
    pub running_turns: usize,
}

/// 进程启动以来的时间
pub fn process_uptime() -> Duration {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// 只取 CPU 与内存：其余信息用不上，免得白白扫描进程、磁盘、网络
fn system() -> Result<System, String> {
    let kinds = RefreshKind::nothing()
        .with_cpu(CpuRefreshKind::nothing().with_usage())
        .with_memory(MemoryRefreshKind::nothing().with_ram());
    System::new_with_specifics(kinds).map_err(|e| format!("读取系统信息失败：{e:?}"))
}

/// 采集主机信息。`name` 为空时用主机名。
pub fn info(name: Option<&str>, flavor: &str) -> Result<MachineInfo, String> {
    let sys = system()?;
    let hostname = discovery::hostname();
    Ok(MachineInfo {
        name: name
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&hostname)
            .to_string(),
        hostname,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        kernel: System::kernel_version().ok(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        flavor: flavor.to_string(),
        home: discovery::home_dir().to_string_lossy().into_owned(),
        uptime_s: System::uptime().unwrap_or(0),
        cpu_count: sys.cpus().len(),
        mem_total: sys.total_memory(),
    })
}

/// 采集此刻的资源占用。会阻塞约 300ms（等 CPU 第二次采样）。
pub async fn metrics(running_turns: usize) -> Result<Metrics, String> {
    let mut sys = system()?;
    tokio::time::sleep(CPU_SAMPLE).await;
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    // Windows 没有平均负载的概念，读不到时记 0
    let LoadAvg { one, five, fifteen } = System::load_average().unwrap_or(LoadAvg {
        one: 0.0,
        five: 0.0,
        fifteen: 0.0,
    });
    Ok(Metrics {
        cpu_percent: sys.global_cpu_usage(),
        mem_used: sys.used_memory(),
        mem_total: sys.total_memory(),
        load1: one as f32,
        load5: five as f32,
        load15: fifteen as f32,
        uptime_s: System::uptime().unwrap_or(0),
        running_turns,
    })
}

/// 按 `.app` 目录路径做精确字面匹配（不用正则，不伤及无关进程）
fn app_needles(app_path: &Path) -> Vec<String> {
    let mut needles = Vec::new();
    let push = |out: &mut Vec<String>, p: &Path| {
        let s = p.to_string_lossy();
        let trimmed = s.trim_end_matches('/');
        if !trimmed.is_empty() {
            let needle = format!("{trimmed}/");
            if !out.contains(&needle) {
                out.push(needle);
            }
        }
    };
    push(&mut needles, app_path);
    if let Ok(canon) = app_path.canonicalize() {
        push(&mut needles, &canon);
    }
    needles
}

fn process_matches_app(proc: &sysinfo::Process, needles: &[String]) -> bool {
    if let Some(exe) = proc.exe().map(|p| p.to_string_lossy())
        && needles.iter().any(|n| exe.starts_with(n.as_str()))
    {
        return true;
    }
    proc.cmd().iter().any(|arg| {
        let s = arg.to_string_lossy();
        needles
            .iter()
            .any(|n| s.starts_with(n.as_str()) || s.contains(n.as_str()))
    })
}

fn scan_processes() -> Option<System> {
    let kinds = RefreshKind::nothing().with_processes(
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    System::new_with_specifics(kinds).ok()
}

/// 指定 `.app` 是否正在运行（精确前缀/字面匹配，避免 `pgrep -f` 正则注入）
pub fn app_running(app_path: &Path) -> bool {
    let needles = app_needles(app_path);
    if needles.is_empty() {
        return false;
    }
    let Some(sys) = scan_processes() else {
        return false;
    };
    let me = std::process::id();
    sys.processes()
        .values()
        .any(|p| p.pid().as_u32() != me && process_matches_app(p, &needles))
}

/// 向属于指定 `.app` 的进程逐一发送 SIGTERM（仅限精确匹配的 PID，避免 `pkill -f` 误杀）
pub fn terminate_app(app_path: &Path) -> usize {
    let needles = app_needles(app_path);
    if needles.is_empty() {
        return 0;
    }
    let Some(sys) = scan_processes() else {
        return 0;
    };
    let me = std::process::id();
    let mut signaled = 0;
    for proc in sys.processes().values() {
        let pid = proc.pid().as_u32();
        if pid <= 1 || pid == me {
            continue;
        }
        if process_matches_app(proc, &needles) && proc.kill_with(Signal::Term).unwrap_or(false) {
            signaled += 1;
        }
    }
    signaled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn metrics_are_sane() {
        let m = metrics(2).await.unwrap();
        assert!(m.cpu_percent >= 0.0 && m.cpu_percent <= 100.0, "{m:?}");
        assert!(m.mem_used <= m.mem_total, "{m:?}");
        assert_eq!(m.running_turns, 2);
    }

    #[test]
    fn info_has_a_name() {
        let i = info(Some("  "), "test").unwrap();
        assert_eq!(i.name, i.hostname);
        assert!(i.cpu_count > 0);
        assert!(i.mem_total > 0);
        assert_eq!(info(Some("home-pc"), "test").unwrap().name, "home-pc");
    }
}

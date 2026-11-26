//! 部署：Vercel 与 Cloudflare（Pages / Workers）的发布管理与守护。
//!
//! - 平台令牌存系统 Keychain；登记的项目（「部署目标」）与部署记录存本地库
//! - 定时拉各目标的部署，归成同一种状态，变化记进活动流；生产部署就绪后做一次健康检查
//! - 生产部署失败、或健康检查不过：取构建日志，自动建一个修复任务交给任务流水线
//!   （修好 → PR → 合并 → 平台重新部署）。同一个目标 24 小时内最多三次
//! - 主动部署、重新部署、取消、提升为生产（Vercel）、回滚
//!
//! 用到的接口都对照官方文档核实过（2026-11）：
//!
//! Vercel（`https://api.vercel.com`，Bearer 令牌；团队用查询串 `teamId`）
//! - 列项目 `GET /v10/projects`；单个项目 `GET /v9/projects/{id}`；当前用户 `GET /v2/user`；团队 `GET /v2/teams`
//! - 列部署 `GET /v7/deployments?projectId=`；单个部署 `GET /v13/deployments/{id}`
//! - 创建 / 重新部署 `POST /v13/deployments`；取消 `PATCH /v12/deployments/{id}/cancel`
//! - 构建日志 `GET /v3/deployments/{id}/events`
//! - 提升为生产 `POST /v10/projects/{projectId}/promote/{deploymentId}`
//! - 回滚 `POST /v1/projects/{projectId}/rollback/{deploymentId}`
//!
//! Cloudflare（`https://api.cloudflare.com/client/v4`，Bearer 令牌）
//! - 账户 `GET /accounts`；Pages 项目 `GET /accounts/{id}/pages/projects`
//! - Pages 部署 `GET …/pages/projects/{name}/deployments`；重试 `POST …/deployments/{id}/retry`；
//!   回滚 `POST …/deployments/{id}/rollback`；构建日志 `GET …/deployments/{id}/history/logs`
//! - Workers `GET /accounts/{id}/workers/scripts`（名字与标签）；构建 `GET /builds/workers/{tag}/builds`；
//!   构建日志 `GET /builds/builds/{uuid}/logs`；取消 `PUT /builds/builds/{uuid}/cancel`
//! - Workers 的部署与版本 `GET|POST …/workers/scripts/{name}/deployments`、`GET …/versions`：
//!   回滚就是把旧版本按 100% 再部署一次
//!
//! Workers Builds 没有「指定分支发起构建」的接口：要走控制台里建的 Deploy Hook（地址存 Keychain）。
//! 这套接口只认用户级令牌，账户级令牌会被拒绝。

use crate::AppState;
use crate::events;
use crate::pipeline;
use crate::remote::{set_setting, setting};
use crate::tasks;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::State;

/// Vercel REST API；应用状态里可以改掉它（测试指向本地的假服务器）
pub const VERCEL_API: &str = "https://api.vercel.com";
/// Cloudflare API v4；同样可以在应用状态里改掉
pub const CF_API: &str = "https://api.cloudflare.com/client/v4";

/// 每个目标一次拉多少条部署
const PAGE: usize = 30;
/// 多久拉一次各个目标的部署
const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// 给 Agent 的构建日志最多带这么多字符
const LOG_TAIL: usize = 12_000;
/// 界面上看日志最多带这么多字符
const LOG_LIMIT: usize = 80_000;
/// 同一个目标在 [`AUTO_FIX_WINDOW_MS`] 里最多自动建几个修复任务
const MAX_AUTO_FIX: i64 = 3;
const AUTO_FIX_WINDOW_MS: i64 = 24 * 3600 * 1000;

// ---------------------------------------------------------------------------
// 平台与数据
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Vercel,
    Pages,
    Workers,
}

impl Provider {
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "vercel" => Ok(Provider::Vercel),
            "cloudflare-pages" => Ok(Provider::Pages),
            "cloudflare-workers" => Ok(Provider::Workers),
            _ => Err(format!("不认识的部署平台：{name}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Provider::Vercel => "vercel",
            Provider::Pages => "cloudflare-pages",
            Provider::Workers => "cloudflare-workers",
        }
    }

    /// 令牌属于哪一家：Pages 与 Workers 共用 Cloudflare 的令牌
    fn platform(self) -> &'static str {
        match self {
            Provider::Vercel => "vercel",
            _ => "cloudflare",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Provider::Vercel => "Vercel",
            Provider::Pages => "Cloudflare Pages",
            Provider::Workers => "Cloudflare Workers",
        }
    }
}

/// 令牌按「家」存：`vercel` / `cloudflare`
fn platform_label(platform: &str) -> Result<&'static str, String> {
    match platform {
        "vercel" => Ok("Vercel"),
        "cloudflare" => Ok("Cloudflare"),
        _ => Err(format!("不认识的平台：{platform}")),
    }
}

fn token_account(platform: &str) -> String {
    format!("deploy_{platform}_token")
}

fn scope_key(platform: &str) -> String {
    format!("deploy_{platform}_scope")
}

fn hook_account(target_id: i64) -> String {
    format!("deploy_hook_{target_id}")
}

/// 部署模块的运行时状态
#[derive(Default)]
pub struct Deployer {
    /// 正在刷新的目标：同一个目标同时只拉一次
    refreshing: Mutex<HashSet<i64>>,
    /// 每个目标最近一次刷新失败的原因；原因没变就不再重复记事件
    last_error: Mutex<HashMap<i64, String>>,
}

/// 登记了的部署目标：Vercel 项目、Cloudflare Pages 项目或 Worker
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    pub id: i64,
    pub provider: String,
    pub name: String,
    /// 平台上的标识：Vercel 的项目 id、Worker 的标签；Pages 项目就是名字
    pub platform_id: String,
    /// 属于哪个团队 / 账户；空表示 Vercel 个人账户
    pub scope: String,
    /// 生产地址
    pub url: Option<String>,
    /// 关联的 GitHub 仓库 `owner/repo`
    pub repo: Option<String>,
    pub repo_id: Option<String>,
    /// 平台那边连的 Git 服务（github …）；空表示没连
    pub git_type: Option<String>,
    /// 生产分支
    pub branch: Option<String>,
    pub framework: Option<String>,
    /// 应用里发起生产部署要先确认
    pub prod_requires_approval: bool,
    /// 生产部署失败时自动建修复任务
    pub auto_fix: bool,
    /// 守护建的修复任务修完直接提交开 PR（验收就变成看 PR）
    pub auto_pr: bool,
    /// PR 合并进生产分支后自动发起部署
    pub auto_deploy: bool,
    /// 健康检查没过时自动回滚到最近健康的版本
    pub auto_rollback: bool,
    pub health_path: String,
    pub health_contains: Option<String>,
    /// Workers：存了 Deploy Hook 的地址
    pub has_hook: bool,
    pub created_at: String,
    pub updated_at: String,
}

const TARGET_COLS: &str = "id, provider, name, platform_id, scope, url, repo, repo_id, git_type, branch, framework, \
     prod_requires_approval, auto_fix, auto_pr, auto_deploy, auto_rollback, health_path, health_contains, has_hook, \
     created_at, updated_at";

fn row_to_target(row: &rusqlite::Row) -> rusqlite::Result<Target> {
    let flag = |i: usize, default: i64| {
        Ok::<bool, rusqlite::Error>(row.get::<_, Option<i64>>(i)?.unwrap_or(default) != 0)
    };
    Ok(Target {
        id: row.get(0)?,
        provider: row.get(1)?,
        name: row.get(2)?,
        platform_id: row.get(3)?,
        scope: row.get(4)?,
        url: row.get(5)?,
        repo: row.get(6)?,
        repo_id: row.get(7)?,
        git_type: row.get(8)?,
        branch: row.get(9)?,
        framework: row.get(10)?,
        prod_requires_approval: flag(11, 1)?,
        auto_fix: flag(12, 1)?,
        auto_pr: flag(13, 0)?,
        auto_deploy: flag(14, 0)?,
        auto_rollback: flag(15, 0)?,
        health_path: row
            .get::<_, Option<String>>(16)?
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "/".to_string()),
        health_contains: row.get(17)?,
        has_hook: flag(18, 0)?,
        created_at: row.get::<_, Option<String>>(19)?.unwrap_or_default(),
        updated_at: row.get::<_, Option<String>>(20)?.unwrap_or_default(),
    })
}

fn load_target(db: &Connection, id: i64) -> Result<Target, String> {
    db.query_row(
        &format!("SELECT {TARGET_COLS} FROM deploy_targets WHERE id = ?1"),
        params![id],
        row_to_target,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("部署目标不存在：#{id}"))
}

fn targets(db: &Connection) -> Result<Vec<Target>, String> {
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {TARGET_COLS} FROM deploy_targets ORDER BY provider, name"
        ))
        .map_err(|e| e.to_string())?;
    stmt.query_map([], row_to_target)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

fn save_target(db: &Connection, target: &Target) -> Result<(), String> {
    db.execute(
        "UPDATE deploy_targets SET name = ?2, platform_id = ?3, scope = ?4, url = ?5, repo = ?6, repo_id = ?7, git_type = ?8, \
         branch = ?9, framework = ?10, prod_requires_approval = ?11, auto_fix = ?12, auto_pr = ?13, auto_deploy = ?14, \
         auto_rollback = ?15, health_path = ?16, health_contains = ?17, has_hook = ?18, updated_at = datetime('now') WHERE id = ?1",
        params![
            target.id,
            target.name,
            target.platform_id,
            target.scope,
            target.url,
            target.repo,
            target.repo_id,
            target.git_type,
            target.branch,
            target.framework,
            target.prod_requires_approval as i64,
            target.auto_fix as i64,
            target.auto_pr as i64,
            target.auto_deploy as i64,
            target.auto_rollback as i64,
            target.health_path,
            target.health_contains,
            target.has_hook as i64,
        ],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// 一次部署：三个平台拉回来后归成同一种样子
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub id: String,
    pub target_id: i64,
    pub provider: String,
    pub project: String,
    /// `production` / `preview`
    pub env: String,
    /// `queued` / `building` / `ready` / `error` / `canceled` / `other`
    pub state: String,
    /// 平台自己给的状态
    pub raw_state: Option<String>,
    pub url: Option<String>,
    /// 平台控制台里这一次部署的页面
    pub inspector_url: Option<String>,
    pub branch: Option<String>,
    pub commit_sha: Option<String>,
    pub commit_message: Option<String>,
    pub author: Option<String>,
    pub created_ms: Option<i64>,
    pub building_ms: Option<i64>,
    pub ready_ms: Option<i64>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    /// 能回滚到这一次
    pub rollback_ok: bool,
    /// 现在线上跑的就是这一次
    pub current: bool,
    /// 到终态后守护已经处理过
    pub guarded: bool,
    /// 健康检查结果：`ok`，或没通过 / 没法检查的原因
    pub health: Option<String>,
    /// 守护为这次失败建的修复任务
    pub task_id: Option<i64>,
}

const DEP_COLS: &str = "id, target_id, provider, project, env, state, raw_state, url, inspector_url, branch, commit_sha, \
     commit_message, author, created_ms, building_ms, ready_ms, error_code, error_message, rollback_ok, current, guarded, \
     health, task_id";

fn row_to_deployment(row: &rusqlite::Row) -> rusqlite::Result<Deployment> {
    let flag =
        |i: usize| Ok::<bool, rusqlite::Error>(row.get::<_, Option<i64>>(i)?.unwrap_or(0) != 0);
    Ok(Deployment {
        id: row.get(0)?,
        target_id: row.get(1)?,
        provider: row.get(2)?,
        project: row.get(3)?,
        env: row.get(4)?,
        state: row.get(5)?,
        raw_state: row.get(6)?,
        url: row.get(7)?,
        inspector_url: row.get(8)?,
        branch: row.get(9)?,
        commit_sha: row.get(10)?,
        commit_message: row.get(11)?,
        author: row.get(12)?,
        created_ms: row.get(13)?,
        building_ms: row.get(14)?,
        ready_ms: row.get(15)?,
        error_code: row.get(16)?,
        error_message: row.get(17)?,
        rollback_ok: flag(18)?,
        current: flag(19)?,
        guarded: flag(20)?,
        health: row.get(21)?,
        task_id: row.get(22)?,
    })
}

fn load_deployment(db: &Connection, id: &str) -> Result<Deployment, String> {
    db.query_row(
        &format!("SELECT {DEP_COLS} FROM deployments WHERE id = ?1"),
        params![id],
        row_to_deployment,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("部署记录不存在：{id}"))
}

/// 从平台拉回来、还没进库的一次部署
#[derive(Clone, Debug)]
struct Fresh {
    id: String,
    env: &'static str,
    state: &'static str,
    raw_state: String,
    /// 已经不会自己再变（成功、失败、取消）
    terminal: bool,
    url: Option<String>,
    inspector_url: Option<String>,
    branch: Option<String>,
    commit_sha: Option<String>,
    commit_message: Option<String>,
    author: Option<String>,
    created_ms: Option<i64>,
    building_ms: Option<i64>,
    ready_ms: Option<i64>,
    error_code: Option<String>,
    error_message: Option<String>,
    rollback_ok: bool,
    current: bool,
}

/// 还没登记的项目
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub provider: String,
    pub name: String,
    pub platform_id: String,
    pub url: Option<String>,
    pub repo: Option<String>,
    pub repo_id: Option<String>,
    pub branch: Option<String>,
    pub framework: Option<String>,
    /// 已经登记过的目标的 id
    pub registered: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderStatus {
    pub configured: bool,
    /// 令牌验证没通过的原因；通过时为空
    pub error: Option<String>,
    pub who: Option<String>,
    /// Vercel 的团队 / Cloudflare 的账户
    pub scopes: Vec<Scope>,
    /// 选中的那一个
    pub scope: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployStatus {
    pub vercel: ProviderStatus,
    pub cloudflare: ProviderStatus,
}

/// 可以回滚到的一次部署
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    /// 守护查过的健康结论；「ok」表示我们亲眼见过它健康
    pub health: Option<String>,
    pub id: String,
    pub created_ms: Option<i64>,
    pub branch: Option<String>,
    pub commit_message: Option<String>,
    /// 现在线上跑的就是这一次
    pub current: bool,
    /// Workers：这一次部署用的版本
    pub version_id: Option<String>,
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

/// 字符串字段；数字、布尔也转成字符串，空串当作没有
fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key) {
        Some(Value::String(s)) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

/// 毫秒时间戳：数字，或是写成字符串的数字
fn ms(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Cloudflare 的时间是 ISO 8601（`2026-11-24T03:00:00.123Z`），换成毫秒时间戳
fn iso_ms(s: &str) -> Option<i64> {
    let num = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let fraction: f64 = match s.get(19..) {
        Some(rest) if rest.starts_with('.') => format!("0.{}", rest[1..].trim_end_matches('Z'))
            .parse()
            .unwrap_or(0.0),
        _ => 0.0,
    };
    // 年月日换成天数（Howard Hinnant 的算法）
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(
        (era * 146097 + doe - 719468) * 86_400_000
            + (hour * 3600 + minute * 60 + second) * 1000
            + (fraction * 1000.0) as i64,
    )
}

fn clip_tail(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let start = text
        .char_indices()
        .rev()
        .nth(max_chars.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    let cut = text[start..].find('\n').map_or(0, |i| i + 1);
    format!("…（前面省略）\n{}", &text[start + cut..])
}

/// `org/repo`，两个都有才算
fn repo_of(org: Option<String>, name: Option<String>) -> Option<String> {
    let (org, name) = (org?, name?);
    (!org.is_empty() && !name.is_empty()).then(|| format!("{org}/{name}"))
}

/// 地址没有协议头就补上 `https://`
fn with_scheme(url: String) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url
    } else {
        format!("https://{url}")
    }
}

fn env_label(env: &str) -> &'static str {
    if env == "production" {
        "生产"
    } else {
        "预览"
    }
}

/// 提交的简写：`abc1234 说明`
fn commit_line(sha: Option<&str>, message: Option<&str>) -> String {
    let short = sha.map(|sha| sha.chars().take(7).collect::<String>());
    let message = message.map(str::trim).filter(|message| !message.is_empty());
    match (short, message) {
        (Some(sha), Some(message)) => format!("{sha} {message}"),
        (Some(sha), None) => sha,
        (None, Some(message)) => message.to_string(),
        (None, None) => "（没有提交信息）".to_string(),
    }
}

fn failure_text(code: Option<&str>, message: Option<&str>, raw_state: Option<&str>) -> String {
    let message = message.map(str::trim).filter(|message| !message.is_empty());
    match (code, message) {
        (Some(code), Some(message)) => format!("{code}：{message}"),
        (Some(code), None) => code.to_string(),
        (None, Some(message)) => message.to_string(),
        (None, None) => format!("构建失败（{}）", raw_state.unwrap_or("ERROR")),
    }
}

/// 健康检查要访问的地址
fn health_url(target: &Target) -> Option<String> {
    let base = target
        .url
        .as_deref()
        .map(|url| url.trim_end_matches('/'))
        .filter(|url| url.starts_with("http"))?;
    let path = if target.health_path.starts_with('/') {
        target.health_path.clone()
    } else {
        format!("/{}", target.health_path)
    };
    Some(format!("{base}{path}"))
}

fn event_link(target_id: i64) -> String {
    format!("#/deploy?t={target_id}")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 记一条部署相关的事件（调用方没有持有数据库锁）
fn record(state: &AppState, kind: &str, level: &str, source: &str, message: &str, link: &str) {
    events::record(
        &state.db.lock().unwrap(),
        kind,
        level,
        source,
        message,
        Some(link),
    );
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

fn client(secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(secs))
        .user_agent(concat!("AutoAgents/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败：{e}"))
}

/// 请求 Vercel。失败时把 `error.message` 带出来
async fn vercel(
    state: &AppState,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let base = state.vercel_api.lock().unwrap().clone();
    let mut request = client(20)?
        .request(
            method
                .parse()
                .map_err(|_| format!("HTTP 方法不对：{method}"))?,
            format!("{base}{path}"),
        )
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let resp = request
        .send()
        .await
        .map_err(|e| format!("连不上 Vercel：{e}"))?;
    let code = resp.status().as_u16();
    let value: Option<Value> = resp.json().await.ok();
    if !(200..300).contains(&code) {
        let error = value.as_ref().and_then(|v| v.get("error"));
        let detail = error
            .and_then(|e| e["message"].as_str())
            .unwrap_or_default();
        let hint = match code {
            401 | 403
                if error
                    .and_then(|e| e["invalidToken"].as_bool())
                    .unwrap_or(false) =>
            {
                "，令牌无效或已过期"
            }
            401 | 403 => "，访问被拒绝：检查令牌的范围（团队 / 项目）",
            404 => "，资源不存在：检查令牌能不能访问这个项目",
            429 => "，请求太频繁，稍后再试",
            _ => "",
        };
        return Err(if detail.is_empty() {
            format!("Vercel API 错误（{code}）{hint}")
        } else {
            format!("Vercel API 错误（{code}）：{detail}{hint}")
        });
    }
    value.ok_or_else(|| "Vercel 返回了空响应".to_string())
}

/// 请求 Cloudflare。HTTP 状态可能是 200 但 `success` 为 false；成功时返回 `result`
async fn cf(
    state: &AppState,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let base = state.cf_api.lock().unwrap().clone();
    let mut request = client(20)?
        .request(
            method
                .parse()
                .map_err(|_| format!("HTTP 方法不对：{method}"))?,
            format!("{base}{path}"),
        )
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let resp = request
        .send()
        .await
        .map_err(|e| format!("连不上 Cloudflare：{e}"))?;
    let status = resp.status().as_u16();
    let value: Option<Value> = resp.json().await.ok();
    let success = value
        .as_ref()
        .and_then(|v| v["success"].as_bool())
        .unwrap_or(false);
    if !success {
        let error = value
            .as_ref()
            .and_then(|v| v["errors"].as_array())
            .and_then(|list| list.first());
        let detail = error
            .and_then(|e| e["message"].as_str())
            .unwrap_or_default();
        let code = error
            .and_then(|e| e["code"].as_i64())
            .map_or_else(|| status.to_string(), |code| code.to_string());
        let hint = match code.as_str() {
            // Authentication error / Invalid access token / Unauthorized to access requested resource
            "10000" | "9109" | "10007" => "（令牌无效，或者没有访问这个资源的权限）",
            _ => "",
        };
        return if detail.is_empty() {
            Err(format!("Cloudflare API 错误（{code}）"))
        } else {
            Err(format!("Cloudflare API 错误（{code}）：{detail}{hint}"))
        };
    }
    Ok(value
        .map(|mut v| v.take()["result"].take())
        .unwrap_or(Value::Null))
}

fn token_of(state: &AppState, provider: Provider) -> Result<String, String> {
    let platform = provider.platform();
    state
        .secrets
        .get(&token_account(platform))?
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| format!("尚未连接 {}：请先在部署页填入 API 令牌", provider.label()))
}

fn scope_of(state: &AppState, platform: &str) -> String {
    setting(&state.db.lock().unwrap(), &scope_key(platform)).unwrap_or_default()
}

/// Vercel 的项目接口把团队写在查询串里
fn with_team(target: &Target, path: &str) -> String {
    if target.scope.is_empty() {
        path.to_string()
    } else if path.contains('?') {
        format!("{path}&teamId={}", target.scope)
    } else {
        format!("{path}?teamId={}", target.scope)
    }
}

/// Cloudflare 上选中的账户；没选过就用令牌能看到的第一个
async fn account_of(state: &AppState, token: &str) -> Result<String, String> {
    let chosen = setting(&state.db.lock().unwrap(), &scope_key("cloudflare"))
        .filter(|scope| !scope.is_empty());
    if let Some(scope) = chosen {
        return Ok(scope);
    }
    let v = cf(state, token, "GET", "/accounts", None).await?;
    v.as_array()
        .and_then(|list| list.first())
        .and_then(|account| text(account, "id"))
        .ok_or_else(|| "这个令牌看不到任何 Cloudflare 账户".to_string())
}

// ---------------------------------------------------------------------------
// 状态归一
// ---------------------------------------------------------------------------

fn vercel_state(raw: &str) -> (&'static str, bool) {
    match raw {
        "QUEUED" | "INITIALIZING" => ("queued", false),
        "BUILDING" => ("building", false),
        "READY" => ("ready", true),
        "ERROR" => ("error", true),
        "CANCELED" | "CANCELLED" => ("canceled", true),
        "BLOCKED" | "DELETED" => ("other", true),
        _ => ("other", false),
    }
}

fn pages_state(stage: &str, skipped: bool) -> (&'static str, bool) {
    if skipped {
        return ("canceled", true);
    }
    match stage {
        "idle" => ("queued", false),
        "active" => ("building", false),
        "success" => ("ready", true),
        "failure" => ("error", true),
        "canceled" | "skipped" => ("canceled", true),
        _ => ("other", false),
    }
}

/// Workers 构建的状态取值官方没有列全：按关键字归类
fn workers_state(status: &str, outcome: &str) -> (&'static str, bool) {
    let has = |words: &[&str]| {
        words
            .iter()
            .any(|word| status.contains(word) || outcome.contains(word))
    };
    if has(&["fail", "error"]) {
        ("error", true)
    } else if has(&["cancel"]) {
        ("canceled", true)
    } else if has(&["skip"]) {
        ("canceled", true)
    } else if has(&["success", "succeed", "complete"]) {
        ("ready", true)
    } else if has(&["queue", "pending", "idle", "waiting"]) {
        ("queued", false)
    } else if has(&["run", "progress", "active", "build"]) {
        ("building", false)
    } else {
        ("other", false)
    }
}

fn from_vercel(v: &Value) -> Option<Fresh> {
    let id = text(v, "uid").or_else(|| text(v, "id"))?;
    let raw = text(v, "readyState")
        .unwrap_or_default()
        .to_ascii_uppercase();
    let (state, terminal) = vercel_state(&raw);
    let production = v["target"].as_str() == Some("production");
    // `meta` 里各个 Git 平台的键官方没有逐一列出来，哪个有用哪个
    let meta = &v["meta"];
    let pick = |github: &str, gitlab: &str| text(meta, github).or_else(|| text(meta, gitlab));
    Some(Fresh {
        env: if production { "production" } else { "preview" },
        current: production && text(v, "readySubstate").is_some_and(|s| s == "PROMOTED"),
        rollback_ok: v["isRollbackCandidate"].as_bool().unwrap_or(false),
        inspector_url: text(v, "inspectorUrl"),
        url: text(v, "url").map(with_scheme),
        branch: pick("githubCommitRef", "gitlabCommitRef"),
        commit_sha: pick("githubCommitSha", "gitlabCommitSha"),
        commit_message: pick("githubCommitMessage", "gitlabCommitMessage"),
        author: pick("githubCommitAuthorName", "gitlabCommitAuthorName"),
        created_ms: ms(v, "created").or_else(|| ms(v, "createdAt")),
        building_ms: ms(v, "buildingAt"),
        ready_ms: ms(v, "ready"),
        error_code: text(v, "errorCode"),
        error_message: text(v, "errorMessage"),
        id,
        state,
        raw_state: raw,
        terminal,
    })
}

/// Cloudflare 的部署：时间都是 ISO 8601，`latest_stage.status` 是当前阶段的状态
fn from_pages(v: &Value) -> Option<Fresh> {
    let id = text(v, "id")?;
    let stage = text(&v["latest_stage"], "status")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (state, terminal) = pages_state(&stage, v["is_skipped"].as_bool().unwrap_or(false));
    let meta = &v["deployment_trigger"]["metadata"];
    let date = |value: &Value, key: &str| text(value, key).and_then(|s| iso_ms(&s));
    // 部署阶段结束的时间才是就绪的时间；没有就退回修改时间
    let ready = v["stages"]
        .as_array()
        .and_then(|stages| {
            stages
                .iter()
                .rev()
                .find(|stage| stage["name"].as_str() == Some("deploy"))
        })
        .and_then(|stage| date(stage, "ended_on"))
        .or_else(|| date(v, "modified_on"));
    Some(Fresh {
        env: if v["environment"].as_str() == Some("production") {
            "production"
        } else {
            "preview"
        },
        current: false,
        rollback_ok: false,
        inspector_url: None,
        url: text(v, "url").map(with_scheme),
        branch: text(meta, "branch"),
        commit_sha: text(meta, "commit_hash"),
        commit_message: text(meta, "commit_message"),
        author: text(meta, "commit_author"),
        created_ms: date(v, "created_on"),
        building_ms: date(v, "build_start_on"),
        ready_ms: ready,
        error_code: None,
        error_message: None,
        id,
        state,
        raw_state: stage,
        terminal,
    })
}

/// Workers 的一次构建。响应的字段官方只给了一部分示例，取不到的留空
fn from_workers(v: &Value, target: &Target) -> Option<Fresh> {
    let id = text(v, "build_uuid").or_else(|| text(v, "id"))?;
    let meta = &v["build_trigger_metadata"];
    let branch = text(meta, "branch");
    let production = match (&target.branch, &branch) {
        (Some(production), Some(branch)) => production == branch,
        _ => text(meta, "environment").as_deref() == Some("production"),
    };
    let status = text(v, "status").unwrap_or_default().to_ascii_lowercase();
    let outcome = text(v, "build_outcome")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (state, terminal) = workers_state(&status, &outcome);
    let date = |keys: &[&str]| {
        keys.iter().find_map(|key| {
            text(v, key)
                .or_else(|| text(meta, key))
                .and_then(|s| iso_ms(&s))
        })
    };
    Some(Fresh {
        env: if production { "production" } else { "preview" },
        current: false,
        rollback_ok: false,
        inspector_url: None,
        // 预览构建的地址平台不一定给；生产构建就是 Worker 自己的地址
        url: if production {
            target.url.clone()
        } else {
            text(meta, "preview_url").map(with_scheme)
        },
        branch,
        commit_sha: text(meta, "commit_hash"),
        commit_message: text(meta, "commit_message"),
        author: text(meta, "commit_author"),
        created_ms: date(&["created_on"]),
        building_ms: date(&["build_start_on", "started_on"]),
        ready_ms: date(&["build_end_on", "finished_on", "modified_on"]),
        error_code: (state == "error")
            .then(|| outcome.clone())
            .filter(|s| !s.is_empty()),
        error_message: text(v, "build_error").or_else(|| text(meta, "error")),
        id,
        state,
        raw_state: if outcome.is_empty() {
            status
        } else {
            format!("{status}/{outcome}")
        },
        terminal,
    })
}

/// 拉一个目标最近的部署
async fn fetch(
    state: &AppState,
    target: &Target,
    provider: Provider,
    token: &str,
) -> Result<Vec<Fresh>, String> {
    match provider {
        Provider::Vercel => {
            let path = with_team(
                target,
                &format!(
                    "/v7/deployments?projectId={}&limit={PAGE}",
                    target.platform_id
                ),
            );
            let v = vercel(state, token, "GET", &path, None).await?;
            Ok(v["deployments"]
                .as_array()
                .map(|list| list.iter().filter_map(from_vercel).collect())
                .unwrap_or_default())
        }
        Provider::Pages => {
            let v = cf(
                state,
                token,
                "GET",
                &format!(
                    "/accounts/{}/pages/projects/{}/deployments?per_page={PAGE}",
                    target.scope, target.name
                ),
                None,
            )
            .await?;
            let mut list: Vec<Fresh> = v
                .as_array()
                .map(|items| items.iter().filter_map(from_pages).collect())
                .unwrap_or_default();
            // 最新的那次成功的生产部署，就是项目当前对外的版本
            if let Some(current) = list
                .iter_mut()
                .find(|dep| dep.env == "production" && dep.state == "ready")
            {
                current.current = true;
            }
            Ok(list)
        }
        Provider::Workers => {
            let v = cf(
                state,
                token,
                "GET",
                &format!(
                    "/accounts/{}/builds/workers/{}/builds?per_page={PAGE}",
                    target.scope, target.platform_id
                ),
                None,
            )
            .await?;
            Ok(v.as_array()
                .or_else(|| v["builds"].as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| from_workers(item, target))
                        .collect()
                })
                .unwrap_or_default())
        }
    }
}

fn upsert(db: &Connection, target: &Target, dep: &Fresh) -> Result<(), String> {
    db.execute(
        "INSERT INTO deployments (id, target_id, provider, project, env, state, raw_state, url, inspector_url, branch, \
         commit_sha, commit_message, author, created_ms, building_ms, ready_ms, error_code, error_message, rollback_ok, current)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT(id) DO UPDATE SET
           target_id = excluded.target_id, state = excluded.state, raw_state = excluded.raw_state,
           url = excluded.url, inspector_url = excluded.inspector_url, branch = excluded.branch,
           commit_sha = excluded.commit_sha, commit_message = excluded.commit_message, author = excluded.author,
           created_ms = excluded.created_ms, building_ms = excluded.building_ms, ready_ms = excluded.ready_ms,
           error_code = excluded.error_code, error_message = excluded.error_message,
           rollback_ok = excluded.rollback_ok, current = excluded.current, updated_at = datetime('now')",
        params![
            dep.id,
            target.id,
            target.provider,
            target.name,
            dep.env,
            dep.state,
            dep.raw_state,
            dep.url,
            dep.inspector_url,
            dep.branch,
            dep.commit_sha,
            dep.commit_message,
            dep.author,
            dep.created_ms,
            dep.building_ms,
            dep.ready_ms,
            dep.error_code,
            dep.error_message,
            dep.rollback_ok as i64,
            dep.current as i64,
        ],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// 刷新与守护
// ---------------------------------------------------------------------------

/// 拉这一个目标最近的部署，到了终态的交给守护
async fn refresh_inner(state: &Arc<AppState>, target: &Target) -> Result<usize, String> {
    let provider = Provider::parse(&target.provider)?;
    let token = token_of(state, provider)?;
    let mut target = target.clone();
    let fresh = fetch(state, &target, provider, &token).await?;

    // Vercel 的项目信息里没有生产地址：从最近一次成功的生产部署上认一个域名
    if provider == Provider::Vercel && target.url.as_deref().is_none_or(|url| url.is_empty()) {
        if let Some(dep) = fresh
            .iter()
            .find(|dep| dep.env == "production" && dep.state == "ready")
        {
            if let Some(url) = production_url(state, &target, &token, &dep.id).await {
                target.url = Some(url);
                save_target(&state.db.lock().unwrap(), &target)?;
            }
        }
    }

    let mut count = 0;
    let mut to_guard = Vec::new();
    {
        let db = state.db.lock().unwrap();
        for dep in &fresh {
            let previous = db
                .query_row(
                    "SELECT state, guarded FROM deployments WHERE id = ?1",
                    params![dep.id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
                        ))
                    },
                )
                .optional()
                .map_err(|e| e.to_string())?;
            upsert(&db, &target, dep)?;
            let stored = load_deployment(&db, &dep.id)?;
            let changed = previous.as_ref().map(|(state, _)| state.as_str()) != Some(dep.state);
            if changed {
                count += 1;
                // 到了终态才往活动流里记：排队、构建中的变化太碎
                let link = stored
                    .inspector_url
                    .clone()
                    .unwrap_or_else(|| event_link(target.id));
                let commit = commit_line(
                    stored.commit_sha.as_deref(),
                    stored.commit_message.as_deref(),
                );
                match dep.state {
                    "ready" => events::record(
                        &db,
                        "deploy.ready",
                        "info",
                        &target.name,
                        &format!("{}部署就绪：{}", env_label(dep.env), commit),
                        Some(&link),
                    ),
                    "error" => events::record(
                        &db,
                        "deploy.failed",
                        "error",
                        &target.name,
                        &format!(
                            "{}部署失败：{}（{}）",
                            env_label(dep.env),
                            commit,
                            failure_text(
                                stored.error_code.as_deref(),
                                stored.error_message.as_deref(),
                                stored.raw_state.as_deref()
                            )
                        ),
                        Some(&link),
                    ),
                    _ => {}
                }
            }
            // 守护只对到了终态、还没处理过的部署做
            if dep.terminal && !previous.as_ref().is_some_and(|(_, guarded)| *guarded) {
                to_guard.push(stored);
            }
        }
    }
    for dep in &to_guard {
        if let Some(plan) = guard(state, &target, dep).await {
            launch_repair(state, plan);
        }
    }
    Ok(count)
}

/// Vercel 上这次部署绑定的域名，当作项目的生产地址
async fn production_url(
    state: &AppState,
    target: &Target,
    token: &str,
    id: &str,
) -> Option<String> {
    let path = with_team(target, &format!("/v13/deployments/{id}"));
    let v = vercel(state, token, "GET", &path, None).await.ok()?;
    let aliases: Vec<&str> = v["alias"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    // 自定义域名优先，其次平台分配的
    let host = aliases
        .iter()
        .find(|alias| !alias.ends_with(".vercel.app"))
        .or_else(|| aliases.first())?;
    Some(format!("https://{host}"))
}

/// 拉这一个目标最近的部署。同一个目标同时只拉一次
pub async fn refresh_target(state: &Arc<AppState>, target: &Target) -> Result<usize, String> {
    if !state.deploy.refreshing.lock().unwrap().insert(target.id) {
        return Ok(0);
    }
    let result = refresh_inner(state, target).await;
    state.deploy.refreshing.lock().unwrap().remove(&target.id);
    match &result {
        Ok(_) => {
            state.deploy.last_error.lock().unwrap().remove(&target.id);
        }
        Err(e) => {
            let mut last = state.deploy.last_error.lock().unwrap();
            // 原因和上次一样就不再重复记，免得活动流里每分钟一条
            if last.get(&target.id) != Some(e) {
                events::record(
                    &state.db.lock().unwrap(),
                    "deploy.refresh",
                    "warning",
                    &target.name,
                    &format!("取不到部署状态：{e}"),
                    Some(&event_link(target.id)),
                );
                last.insert(target.id, e.clone());
            }
        }
    }
    result
}

/// 拉全部目标（或指定的那一个）的部署，返回状态有变化的条数
pub async fn refresh(state: &Arc<AppState>, target_id: Option<i64>) -> Result<usize, String> {
    let list = {
        let db = state.db.lock().unwrap();
        match target_id {
            Some(id) => vec![load_target(&db, id)?],
            None => targets(&db)?,
        }
    };
    let mut count = 0;
    let mut first_error: Option<String> = None;
    for target in &list {
        match refresh_target(state, target).await {
            Ok(n) => count += n,
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(count),
    }
}

/// 应用启动后调用：先拉一遍，然后定期刷新各个目标
pub fn start_polling(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let _ = refresh(&state, None).await;
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let _ = refresh(&state, None).await;
        }
    });
}

/// 守护为一次失败的部署做了什么：建好的修复任务，以及开工要用的设置
pub struct RepairPlan {
    pub task_id: i64,
    pub opts: pipeline::StartOptions,
}

/// 守护：生产部署到了终态后查一次健康；出了问题、且这个目标开了自动修复，就建一个修复任务。
/// 每次部署只走一遍。
async fn guard(state: &Arc<AppState>, target: &Target, dep: &Deployment) -> Option<RepairPlan> {
    let production = dep.env == "production";
    let mut problem = (production && dep.state == "error").then(|| {
        failure_text(
            dep.error_code.as_deref(),
            dep.error_message.as_deref(),
            dep.raw_state.as_deref(),
        )
    });
    let mut health = None;
    // 只有「部署成功但健康没过」才需要回滚：构建失败时线上还是旧版本
    let mut health_failed = false;
    if production && dep.state == "ready" {
        let url = health_url(target);
        let verdict = match &url {
            Some(url) => check(url, target.health_contains.as_deref()).await,
            None => Health::Unknown("没有生产地址，没法检查".to_string()),
        };
        match &verdict {
            Health::Ok => health = Some("ok".to_string()),
            Health::Unknown(text) => {
                events::record(
                    &state.db.lock().unwrap(),
                    "deploy.health",
                    "warning",
                    &target.name,
                    &format!("生产部署好了，但健康检查没法做：{text}"),
                    Some(&event_link(target.id)),
                );
                health = Some(text.clone());
            }
            Health::Fail(reason) => {
                health = Some(reason.clone());
                health_failed = true;
                let checked = url.clone().unwrap_or_default();
                problem = Some(format!(
                    "部署成功了，但健康检查没通过：{reason}（查的是 {checked}）"
                ));
            }
        }
    }
    {
        let db = state.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE deployments SET health = ?1, guarded = 1 WHERE id = ?2",
            params![health, dep.id],
        );
    }
    // 健康没过且开了自动回滚：先止血，修复任务照建（两个开关独立）
    if health_failed && target.auto_rollback {
        rollback_to_healthy(state, target, dep).await;
    }
    let reason = problem?;

    // 预览每次推送都会来一次，太吵：只处理生产环境的
    let repo = {
        let db = state.db.lock().unwrap();
        let link = event_link(target.id);
        if !target.auto_fix {
            events::record(
                &db,
                "deploy.guard",
                "warning",
                &target.name,
                &format!("生产部署出了问题，但这个目标没有开自动修复：{reason}"),
                Some(&link),
            );
            return None;
        }
        let Some(repo) = target.repo.clone() else {
            events::record(
                &db,
                "deploy.guard",
                "warning",
                &target.name,
                &format!(
                    "生产部署出了问题，但这个项目没有关联 GitHub 仓库，没法自动建修复任务：{reason}"
                ),
                Some(&link),
            );
            return None;
        };
        let since = now_ms() - AUTO_FIX_WINDOW_MS;
        let fixed: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM deployments WHERE target_id = ?1 AND task_id IS NOT NULL AND COALESCE(created_ms, 0) > ?2",
                params![target.id, since],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if fixed >= MAX_AUTO_FIX {
            events::record(
                &db,
                "deploy.guard",
                "warning",
                &target.name,
                &format!(
                    "生产部署又失败了。24 小时内已经自动建过 {MAX_AUTO_FIX} 个修复任务，不再自动建，请人工看一下：{reason}"
                ),
                Some(&link),
            );
            return None;
        }
        repo
    };
    // 构建失败才带构建日志；健康检查不过时构建是成功的，日志帮不上忙
    let logs = if dep.state == "error" {
        logs(state, dep).await.unwrap_or_default()
    } else {
        String::new()
    };

    let db = state.db.lock().unwrap();
    let short = dep
        .commit_sha
        .as_deref()
        .or(Some(dep.id.as_str()))
        .map(|text| text.chars().take(7).collect::<String>())
        .unwrap_or_default();
    let title = format!("修复 {} 部署失败（{short}）", target.name);
    let instructions = repair_instructions(target, dep, &reason, &logs);
    let task = match tasks::create_manual(&db, &title, &repo, &instructions) {
        Ok(task) => task,
        Err(e) => {
            events::record(
                &db,
                "deploy.guard",
                "error",
                &target.name,
                &format!("建修复任务失败：{e}"),
                Some(&event_link(target.id)),
            );
            return None;
        }
    };
    let _ = db.execute(
        "UPDATE deployments SET task_id = ?1 WHERE id = ?2",
        params![task.id, dep.id],
    );
    events::record(
        &db,
        "deploy.repair",
        "warning",
        &target.name,
        &format!("生产部署出了问题，已建修复任务 #{}：{reason}", task.id),
        Some(&format!("#/tasks?t={}", task.id)),
    );
    // 上次在哪个机器上、用哪个 Agent 处理过这个仓库：有的话直接开工
    previous_settings(&db, &repo).map(|mut opts| {
        // 基于出问题的那条分支修；修完直接开 PR 的话按目标上的开关来
        opts.base_branch = dep.branch.clone();
        opts.auto_pr = target.auto_pr;
        RepairPlan {
            task_id: task.id,
            opts,
        }
    })
}

/// 健康没过时自动回滚：优先回到我们亲眼见过健康的部署，没有这样的候选就回到最近一次成功的；
/// 什么都没有就只交给修复任务。回滚到的版本要是也坏，守护会对它再查一次、再回滚一次——
/// 每一步都有事件，修复任务的 24 小时上限也拦着失控。
async fn rollback_to_healthy(state: &Arc<AppState>, target: &Target, dep: &Deployment) {
    let items = match history(state, target.id).await {
        Ok(items) => items,
        Err(e) => {
            record(
                state,
                "deploy.rollback",
                "error",
                &target.name,
                &format!("健康检查没过，但拿不到可回滚的部署列表：{e}"),
                &event_link(target.id),
            );
            return;
        }
    };
    let candidate = items
        .iter()
        .find(|item| item.id != dep.id && item.health.as_deref() == Some("ok"))
        .or_else(|| items.iter().find(|item| item.id != dep.id));
    let Some(candidate) = candidate else {
        record(
            state,
            "deploy.rollback",
            "warning",
            &target.name,
            "健康检查没过，但没有可以回滚的部署，先交给修复任务",
            &event_link(target.id),
        );
        return;
    };
    let note = candidate
        .commit_message
        .clone()
        .map(|msg| format!("（{msg}）"))
        .unwrap_or_default();
    // 回滚完会刷新部署、又走一遍守护：把这一步 box 住，异步递归才有界
    match Box::pin(rollback(
        state,
        target.id,
        &candidate.id,
        candidate.version_id.as_deref(),
    ))
    .await
    {
        Ok(()) => record(
            state,
            "deploy.auto",
            "warning",
            &target.name,
            &format!("健康检查没过，先自动回滚止血{note}，修复任务照建"),
            &event_link(target.id),
        ),
        Err(e) => record(
            state,
            "deploy.rollback",
            "error",
            &target.name,
            &format!("自动回滚没成功：{e}"),
            &event_link(target.id),
        ),
    }
}

/// PR 合并进生产分支后：开了「合并即部署」的目标自动发起部署。
/// 平台自己已经在部署这个提交（连了 Git 的 Vercel / Pages 推上去就会部署）就不重复触发；
/// 目标没记生产分支的话没法判断这次合并该不该部署，也不触发。
pub async fn on_pr_merged(state: &Arc<AppState>, repo: &str, branch: &str, sha: Option<&str>) {
    let all = {
        let db = state.db.lock().unwrap();
        targets(&db).unwrap_or_default()
    };
    for target in all {
        if !target.auto_deploy || target.repo.as_deref() != Some(repo) {
            continue;
        }
        let Some(production) = target
            .branch
            .as_deref()
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
        else {
            record(
                state,
                "deploy.auto",
                "warning",
                &target.name,
                "PR 合并了，但这个目标没有记生产分支，没法判断该不该自动部署",
                &event_link(target.id),
            );
            continue;
        };
        if production != branch {
            continue;
        }
        // 平台自己已经在部署这个提交了吗（连了 Git 的 Vercel / Pages 推上去就部署了）
        if let Some(sha) = sha.filter(|sha| !sha.is_empty()) {
            let already = list(state, Some(target.id), Some(50))
                .unwrap_or_default()
                .into_iter()
                .any(|dep| dep.commit_sha.as_deref() == Some(sha));
            if already {
                record(
                    state,
                    "deploy.auto",
                    "info",
                    &target.name,
                    &format!("PR 合并了，平台已经在部署 {sha}，不重复触发"),
                    &event_link(target.id),
                );
                continue;
            }
        }
        // 开着「合并即部署」就视为这次生产部署已确认
        match trigger(state, target.id, Some(branch), sha, true).await {
            Ok(_) => record(
                state,
                "deploy.auto",
                "info",
                &target.name,
                &format!("PR 合并进 {branch}，自动发起了部署"),
                &event_link(target.id),
            ),
            Err(e) => record(
                state,
                "deploy.auto",
                "warning",
                &target.name,
                &format!("PR 合并了，但自动部署没发出去：{e}"),
                &event_link(target.id),
            ),
        }
    }
}

/// 这个仓库最近一次任务用的机器、Agent 与仓库位置
pub(crate) fn previous_settings(db: &Connection, repo: &str) -> Option<pipeline::StartOptions> {
    let id = db
        .query_row(
            "SELECT id FROM tasks WHERE repo = ?1 AND node_id IS NOT NULL AND adapter IS NOT NULL AND workdir IS NOT NULL \
             ORDER BY id DESC LIMIT 1",
            params![repo],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .ok()??;
    let task = tasks::load(db, id).ok()?;
    Some(pipeline::StartOptions {
        node_id: task.node_id?,
        adapter: task.adapter?,
        model: task.model,
        workdir: task.workdir?,
        base_branch: None,
        // 修复也要先让人看过改动
        auto_pr: false,
    })
}

/// 把修复任务交给任务流水线开工（准备工作树要跑 git，放到后台去）
fn launch_repair(state: &Arc<AppState>, plan: RepairPlan) {
    let state = Arc::clone(state);
    tauri::async_runtime::spawn(async move {
        let task_id = plan.task_id;
        // 流水线启动不了时任务会直接记为失败，这里把它失败的原因也报到活动流里
        let failed = match pipeline::start(&state, task_id, &plan.opts).await {
            Ok(task) if task.status == "failed" => task.failure.unwrap_or_default(),
            Ok(_) => return,
            Err(e) => e,
        };
        record(
            &state,
            "deploy.repair",
            "warning",
            "部署守护",
            &format!("修复任务 #{task_id} 没能自动开工（可以在任务页手动启动）：{failed}"),
            &format!("#/tasks?t={task_id}"),
        );
    });
}

/// 交给 Agent 的问题说明。任务流水线会把它当作「要做的事」，前后还有通用的要求
fn repair_instructions(target: &Target, dep: &Deployment, reason: &str, logs: &str) -> String {
    let provider = Provider::parse(&target.provider)
        .map(Provider::label)
        .unwrap_or("部署平台");
    let mut text = format!(
        "{} 上的项目「{}」的{}部署出了问题，请找出原因并修好。\n\n\
         - 分支：{}\n\
         - 提交：{}\n\
         - 问题：{reason}\n",
        provider,
        target.name,
        env_label(&dep.env),
        dep.branch.as_deref().unwrap_or("（不知道）"),
        commit_line(dep.commit_sha.as_deref(), dep.commit_message.as_deref())
    );
    if !logs.trim().is_empty() {
        text.push_str("\n构建日志的最后部分：\n\n```\n");
        text.push_str(&clip_tail(logs, LOG_TAIL));
        text.push_str("\n```\n");
    }
    text.push_str(
        "\n先在本地把构建重新跑一遍（一般在仓库根目录执行构建命令）复现问题，定位到出错的文件后修改，\
         改完确认构建能过。如果问题不在代码里（环境变量、平台上的配置之类），最后说明里写清楚该去哪里改。",
    );
    text
}

// ---------------------------------------------------------------------------
// 健康检查
// ---------------------------------------------------------------------------

enum Health {
    Ok,
    Fail(String),
    /// 没法判断（地址有访问保护之类），不算失败
    Unknown(String),
}

/// 访问一次生产地址：2xx 才算通过；设了期望内容的话，返回里还要包含它
async fn check(url: &str, contains: Option<&str>) -> Health {
    let client = match client(10) {
        Ok(client) => client,
        Err(e) => return Health::Fail(e),
    };
    let resp = match client.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            // 连不上多半是这台机器的网络问题（平台那边总有人接），不算部署出事；超时才算
            return if e.is_timeout() {
                Health::Fail("请求超时（10 秒）".to_string())
            } else {
                Health::Unknown(format!("连不上生产地址：{}", e.without_url()))
            };
        }
    };
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Health::Unknown(format!("HTTP {status}：这个地址有访问保护，检查不了"));
    }
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Health::Fail(format!("HTTP {status}"));
    }
    if let Some(needle) = contains.map(str::trim).filter(|needle| !needle.is_empty()) {
        if !body.contains(needle) {
            return Health::Fail(format!("返回的内容里没有「{needle}」"));
        }
    }
    Health::Ok
}

// ---------------------------------------------------------------------------
// 操作
// ---------------------------------------------------------------------------
async fn status(state: &AppState, platform: &str) -> ProviderStatus {
    let mut status = ProviderStatus {
        configured: false,
        error: None,
        who: None,
        scopes: Vec::new(),
        scope: None,
    };
    if let Err(e) = platform_label(platform) {
        status.error = Some(e);
        return status;
    }
    let account = token_account(platform);
    let token = match state.secrets.get(&account) {
        Ok(token) => token.filter(|token| !token.trim().is_empty()),
        Err(e) => {
            status.error = Some(e);
            return status;
        }
    };
    let Some(token) = token else {
        return status;
    };
    status.configured = true;
    if platform == "vercel" {
        // 能列出项目就算令牌有效
        match vercel(state, &token, "GET", "/v10/projects?limit=1", None).await {
            Ok(_) => {}
            Err(e) => {
                status.error = Some(e);
                return status;
            }
        }
        if let Ok(v) = vercel(state, &token, "GET", "/v2/user", None).await {
            let user = &v["user"];
            status.who = text(user, "username").or_else(|| text(user, "email"));
            status.scope = text(user, "defaultTeamId");
        }
        if let Ok(v) = vercel(state, &token, "GET", "/v2/teams", None).await {
            status.scopes = scopes_from(&v["teams"], "id", &["name", "slug"]);
        }
    } else {
        match cf(state, &token, "GET", "/accounts", None).await {
            Ok(v) => {
                status.scopes = scopes_from(&v, "id", &["name"]);
                status.scope = status.scopes.first().map(|scope| scope.id.clone());
            }
            Err(e) => {
                status.error = Some(e);
                return status;
            }
        }
    }
    if let Some(saved) =
        setting(&state.db.lock().unwrap(), &scope_key(platform)).filter(|saved| !saved.is_empty())
    {
        status.scope = Some(saved);
    }
    status
}

fn scopes_from(list: &Value, id: &str, names: &[&str]) -> Vec<Scope> {
    list.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let id = text(item, id)?;
                    let name = names.iter().find_map(|name| text(item, name))?;
                    Some(Scope { id, name })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 填入令牌：先验证，通过了才存进 Keychain
pub async fn connect(
    state: &Arc<AppState>,
    platform: &str,
    token: &str,
) -> Result<ProviderStatus, String> {
    let label = platform_label(platform)?;
    let token = token.trim();
    if token.is_empty() {
        return Err("请输入 API 令牌".to_string());
    }
    let account = token_account(platform);
    state.secrets.set(&account, token)?;
    let status = status(state, platform).await;
    match status.error {
        Some(error) => {
            let _ = state.secrets.delete(&account);
            Err(error)
        }
        None => {
            let who = status
                .who
                .as_deref()
                .map(|who| format!("（{who}）"))
                .unwrap_or_default();
            // 还没选过团队 / 账户的话，就把这次认出来的记下来
            if let Some(scope) = status.scope.as_deref().filter(|scope| !scope.is_empty()) {
                if scope_of(state, platform).is_empty() {
                    set_setting(&state.db.lock().unwrap(), &scope_key(platform), scope)?;
                }
            }
            record(
                state,
                "deploy.connected",
                "info",
                label,
                &format!("{label} 已连接{who}"),
                "#/deploy",
            );
            Ok(status)
        }
    }
}

pub fn disconnect(state: &AppState, platform: &str) -> Result<(), String> {
    let label = platform_label(platform)?;
    state.secrets.delete(&token_account(platform))?;
    let db = state.db.lock().unwrap();
    set_setting(&db, &scope_key(platform), "")?;
    events::record(
        &db,
        "deploy.disconnected",
        "info",
        label,
        &format!("{label} 已断开连接"),
        Some("#/deploy"),
    );
    Ok(())
}

/// 选定在哪个团队 / 账户下工作；留空用默认的
pub fn set_scope(state: &AppState, platform: &str, scope: Option<&str>) -> Result<(), String> {
    platform_label(platform)?;
    let scope = scope.map(str::trim).unwrap_or_default();
    set_setting(&state.db.lock().unwrap(), &scope_key(platform), scope)
}

/// 没登记的项目：Vercel 的项目，或 Cloudflare 的 Pages 项目与 Worker
pub async fn discover(state: &Arc<AppState>, platform: &str) -> Result<Vec<Candidate>, String> {
    let label = platform_label(platform)?;
    let token = state
        .secrets
        .get(&token_account(platform))?
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| format!("尚未连接 {label}：请先填入 API 令牌"))?;
    let registered: HashMap<(String, String), i64> = targets(&state.db.lock().unwrap())?
        .into_iter()
        .map(|target| ((target.provider, target.platform_id), target.id))
        .collect();
    let mut list: Vec<Candidate> = Vec::new();
    if platform == "vercel" {
        let path = match scope_of(state, platform) {
            scope if !scope.is_empty() => format!("/v10/projects?limit=100&teamId={scope}"),
            _ => "/v10/projects?limit=100".to_string(),
        };
        let v = vercel(state, &token, "GET", &path, None).await?;
        for project in v
            .as_array()
            .or_else(|| v["projects"].as_array())
            .cloned()
            .unwrap_or_default()
        {
            let Some(name) = text(&project, "name") else {
                continue;
            };
            let link = &project["link"];
            list.push(Candidate {
                platform_id: text(&project, "id").unwrap_or_else(|| name.clone()),
                name,
                url: None,
                repo: repo_of(text(link, "org"), text(link, "repo")),
                repo_id: text(link, "repoId"),
                branch: text(link, "productionBranch"),
                framework: text(&project, "framework"),
                provider: "vercel".to_string(),
                registered: None,
            });
        }
    } else {
        let account = account_of(state, &token).await?;
        let pages = cf(
            state,
            &token,
            "GET",
            &format!("/accounts/{account}/pages/projects?per_page=100"),
            None,
        )
        .await?;
        for project in pages.as_array().cloned().unwrap_or_default() {
            let Some(name) = text(&project, "name") else {
                continue;
            };
            let source = &project["source"]["config"];
            list.push(Candidate {
                platform_id: name.clone(),
                url: project["domains"]
                    .as_array()
                    .and_then(|domains| domains.first())
                    .and_then(Value::as_str)
                    .map(|domain| with_scheme(domain.to_string()))
                    .or_else(|| {
                        text(&project, "subdomain").map(|sub| format!("https://{name}.{sub}"))
                    }),
                repo: repo_of(text(source, "owner"), text(source, "repo_name")),
                repo_id: text(source, "repo_id"),
                branch: text(source, "production_branch")
                    .or_else(|| text(&project, "production_branch")),
                framework: text(&project, "framework"),
                provider: "cloudflare-pages".to_string(),
                name,
                registered: None,
            });
        }
        // 令牌没有 Workers 的读取权限时列不出 Worker：不算错，Pages 照样能用
        match cf(
            state,
            &token,
            "GET",
            &format!("/accounts/{account}/workers/scripts"),
            None,
        )
        .await
        {
            Ok(v) => list.extend(v.as_array().cloned().unwrap_or_default().iter().filter_map(
                |script| {
                    let name = text(script, "id")?;
                    Some(Candidate {
                        platform_id: text(script, "tag").unwrap_or_else(|| name.clone()),
                        name,
                        url: None,
                        repo: None,
                        repo_id: None,
                        branch: None,
                        framework: None,
                        provider: "cloudflare-workers".to_string(),
                        registered: None,
                    })
                },
            )),
            Err(e) => events::record(
                &state.db.lock().unwrap(),
                "deploy.discover",
                "warning",
                "Cloudflare",
                &format!("列不出 Worker（令牌需要 Workers Scripts 的读取权限）：{e}"),
                Some("#/deploy"),
            ),
        }
    }
    for candidate in &mut list {
        let key = (candidate.provider.clone(), candidate.platform_id.clone());
        candidate.registered = registered.get(&key).copied();
    }
    list.sort_by_key(|candidate| (candidate.provider.clone(), candidate.name.to_lowercase()));
    Ok(list)
}

/// 登记时向平台补全信息：关联的仓库、生产分支、生产地址
async fn enrich(state: &AppState, target: &mut Target, token: &str) {
    let pages = |tail: &str| pages_path(target, tail);
    match Provider::parse(&target.provider) {
        Ok(Provider::Vercel) => {
            let path = with_team(target, &format!("/v9/projects/{}", target.platform_id));
            if let Ok(v) = vercel(state, token, "GET", &path, None).await {
                let link = &v["link"];
                target.repo = repo_of(text(link, "org"), text(link, "repo"));
                target.repo_id = text(link, "repoId");
                target.git_type = text(link, "type");
                target.branch = text(link, "productionBranch");
                target.framework = text(&v, "framework");
            }
        }
        Ok(Provider::Pages) => {
            if let Ok(v) = cf(state, token, "GET", &pages(""), None).await {
                let source = &v["source"]["config"];
                if target.url.is_none() {
                    target.url = v["domains"]
                        .as_array()
                        .and_then(|domains| domains.first())
                        .and_then(Value::as_str)
                        .map(|domain| with_scheme(domain.to_string()))
                        .or_else(|| {
                            text(&v, "subdomain")
                                .map(|sub| format!("https://{}.{}", target.name, sub))
                        });
                }
                target.repo = repo_of(text(source, "owner"), text(source, "repo_name"));
                target.repo_id = text(source, "repo_id");
                target.git_type = text(&v["source"], "type");
                target.branch =
                    text(source, "production_branch").or_else(|| text(&v, "production_branch"));
                target.framework = text(&v, "framework");
            }
        }
        Ok(Provider::Workers) => {
            // 构建设置里有连的仓库与生产分支
            let settings = format!(
                "/accounts/{}/builds/workers/{}",
                target.scope, target.platform_id
            );
            if let Ok(v) = cf(state, token, "GET", &settings, None).await {
                let repo = &v["git_repository"];
                target.repo = text(repo, "repo_name")
                    .or_else(|| text(repo, "full_name"))
                    .map(|repo| {
                        repo.rsplit("github.com/")
                            .next()
                            .unwrap_or(&repo)
                            .trim_end_matches(".git")
                            .to_string()
                    })
                    .filter(|repo| repo.contains('/'));
                target.branch = text(repo, "branch");
                target.git_type = text(repo, "provider")
                    .or_else(|| Some("github".to_string()).filter(|_| target.repo.is_some()));
            }
            if target.url.is_none() {
                if let Ok(v) = cf(
                    state,
                    token,
                    "GET",
                    &format!("/accounts/{}/workers/subdomain", target.scope),
                    None,
                )
                .await
                {
                    if let Some(sub) = text(&v, "subdomain") {
                        target.url = Some(format!("https://{}.{}.workers.dev", target.name, sub));
                    }
                }
            }
        }
        Err(_) => {}
    }
}

fn pages_path(target: &Target, tail: &str) -> String {
    let base = format!("/accounts/{}/pages/projects/{}", target.scope, target.name);
    if tail.is_empty() {
        base
    } else {
        format!("{base}/{tail}")
    }
}

/// 登记一个部署目标。已经登记过的直接返回原来那个。
/// `url` 给了就用它当生产地址，没给就向平台认
pub async fn add_target(
    state: &Arc<AppState>,
    provider: &str,
    name: &str,
    platform_id: &str,
    url: Option<&str>,
    hook_url: Option<&str>,
) -> Result<Target, String> {
    let provider = Provider::parse(provider)?;
    let name = name.trim();
    if name.is_empty() {
        return Err("项目名不能为空".to_string());
    }
    let token = token_of(state, provider)?;
    let scope = if provider == Provider::Vercel {
        scope_of(state, "vercel")
    } else {
        account_of(state, &token).await?
    };
    let existing = targets(&state.db.lock().unwrap())?
        .into_iter()
        .find(|target| target.provider == provider.as_str() && target.platform_id == platform_id);
    if let Some(target) = existing {
        return Ok(target);
    }
    let mut target = {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO deploy_targets (provider, name, platform_id, scope, health_path) VALUES (?1, ?2, ?3, ?4, '/')",
            params![provider.as_str(), name, platform_id.trim(), scope],
        )
        .map_err(|e| e.to_string())?;
        load_target(&db, db.last_insert_rowid())?
    };
    target.url = url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(|url| with_scheme(url.to_string()));
    record(
        state,
        "deploy.target",
        "info",
        &target.name,
        &format!("已登记部署目标（{}）", provider.label()),
        &event_link(target.id),
    );
    if let Some(hook) = hook_url
        .map(str::trim)
        .filter(|hook| hook.starts_with("https://"))
    {
        state.secrets.set(&hook_account(target.id), hook)?;
        target.has_hook = true;
    }
    enrich(state, &mut target, &token).await;
    save_target(&state.db.lock().unwrap(), &target)?;
    // 顺手拉一次部署
    let _ = refresh_target(state, &target).await;
    load_target(&state.db.lock().unwrap(), target.id)
}

/// 改目标设置：传了哪项改哪项
#[allow(clippy::too_many_arguments)]
pub fn update_target(
    state: &AppState,
    id: i64,
    url: Option<&str>,
    prod_requires_approval: Option<bool>,
    auto_fix: Option<bool>,
    auto_pr: Option<bool>,
    auto_deploy: Option<bool>,
    auto_rollback: Option<bool>,
    health_path: Option<&str>,
    health_contains: Option<&str>,
    hook_url: Option<&str>,
) -> Result<Target, String> {
    let mut target = load_target(&state.db.lock().unwrap(), id)?;
    if let Some(url) = url.map(str::trim).filter(|url| !url.is_empty()) {
        target.url = Some(with_scheme(url.to_string()));
    }
    if let Some(flag) = prod_requires_approval {
        target.prod_requires_approval = flag;
    }
    if let Some(flag) = auto_fix {
        target.auto_fix = flag;
    }
    if let Some(flag) = auto_pr {
        target.auto_pr = flag;
    }
    if let Some(flag) = auto_deploy {
        target.auto_deploy = flag;
    }
    if let Some(flag) = auto_rollback {
        target.auto_rollback = flag;
    }
    if let Some(path) = health_path.map(str::trim).filter(|path| !path.is_empty()) {
        target.health_path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
    }
    if let Some(needle) = health_contains {
        target.health_contains =
            Some(needle.trim().to_string()).filter(|needle| !needle.is_empty());
    }
    match hook_url.map(str::trim) {
        Some("") => {
            state.secrets.delete(&hook_account(id))?;
            target.has_hook = false;
        }
        Some(hook) if hook.starts_with("https://") => {
            state.secrets.set(&hook_account(id), hook)?;
            target.has_hook = true;
        }
        Some(_) => return Err("Deploy Hook 的地址应该以 https:// 开头".to_string()),
        None => {}
    }
    save_target(&state.db.lock().unwrap(), &target)?;
    Ok(target)
}

/// 取消登记（它的部署记录一并删掉；平台上的项目不动）
pub fn remove_target(state: &AppState, id: i64) -> Result<(), String> {
    let target = {
        let db = state.db.lock().unwrap();
        let target = load_target(&db, id)?;
        db.execute("DELETE FROM deployments WHERE target_id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        db.execute("DELETE FROM deploy_targets WHERE id = ?1", params![id])
            .map_err(|e| e.to_string())?;
        events::record(
            &db,
            "deploy.target",
            "info",
            &target.name,
            "已取消登记部署目标",
            Some("#/deploy"),
        );
        target
    };
    let _ = state.secrets.delete(&hook_account(id));
    let _ = target;
    Ok(())
}

/// 本地记着的部署，新的在前
pub fn list(
    state: &AppState,
    target_id: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<Deployment>, String> {
    let limit = limit.unwrap_or(50).clamp(1, 500);
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {DEP_COLS} FROM deployments WHERE (?1 IS NULL OR target_id = ?1) \
             ORDER BY created_ms DESC LIMIT ?2"
        ))
        .map_err(|e| e.to_string())?;
    stmt.query_map(params![target_id, limit], row_to_deployment)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// 这个任务的分支（或它推上去的提交）对应的部署；守护为某次部署建的修复任务，在推提交之前就靠 task_id 对上
pub fn for_task(state: &AppState, task_id: i64) -> Result<Vec<Deployment>, String> {
    let db = state.db.lock().unwrap();
    let task = tasks::load(&db, task_id)?;
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {} FROM deployments d JOIN deploy_targets t ON t.id = d.target_id \
             WHERE d.task_id = ?4 OR (t.repo = ?1 AND (d.branch = ?2 OR (?3 IS NOT NULL AND d.commit_sha = ?3))) \
             ORDER BY d.created_ms DESC LIMIT 20",
            DEP_COLS.split(", ").map(|column| format!("d.{column}")).collect::<Vec<_>>().join(", ")
        ))
        .map_err(|e| e.to_string())?;
    stmt.query_map(
        params![task.repo, task.branch, task.head_sha, task_id],
        row_to_deployment,
    )
    .map_err(|e| e.to_string())?
    .collect::<Result<Vec<_>, _>>()
    .map_err(|e| e.to_string())
}

/// 触发 Worker 的 Deploy Hook。地址本身就是凭据，不需要鉴权
async fn fire_hook(state: &AppState, target: &Target) -> Result<(), String> {
    let hook = state
        .secrets
        .get(&hook_account(target.id))?
        .filter(|hook| !hook.trim().is_empty())
        .ok_or_else(|| {
            "这个 Worker 没有配 Deploy Hook：在 Cloudflare 控制台「Settings → Builds → Deploy Hooks」里建一个，填到目标设置里"
                .to_string()
        })?;
    let resp = client(20)?
        .post(&hook)
        .send()
        .await
        .map_err(|e| format!("触发 Deploy Hook 失败：{}", e.without_url()))?;
    if !resp.status().is_success() {
        return Err(format!(
            "Deploy Hook 返回了 HTTP {}",
            resp.status().as_u16()
        ));
    }
    Ok(())
}

/// 主动部署。Vercel 按分支（或指定的提交）发一次；Pages 把这条分支最近的一次原样再来一遍；
/// Workers 触发 Deploy Hook（构建哪条分支由 Hook 决定）
pub async fn trigger(
    state: &Arc<AppState>,
    target_id: i64,
    branch: Option<&str>,
    commit: Option<&str>,
    confirmed: bool,
) -> Result<Deployment, String> {
    let target = load_target(&state.db.lock().unwrap(), target_id)?;
    let provider = Provider::parse(&target.provider)?;
    let token = token_of(state, provider)?;
    let branch = branch
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .or_else(|| target.branch.clone());
    let commit = commit.map(str::trim).filter(|commit| !commit.is_empty());

    let gate = |production: bool| {
        if production && target.prod_requires_approval && !confirmed {
            Err("这是生产部署，需要先确认".to_string())
        } else {
            Ok(())
        }
    };
    let id = match provider {
        Provider::Vercel => {
            let branch = branch
                .clone()
                .ok_or("这个项目没有记录生产分支，请指定要部署的分支")?;
            let production = target.branch.as_deref() == Some(branch.as_str());
            gate(production)?;
            let mut body = json!({ "name": target.name, "project": target.platform_id });
            let github = target.git_type.as_deref() == Some("github")
                && (target.repo.is_some() || target.repo_id.is_some());
            if github {
                let mut source = json!({ "type": "github", "ref": branch });
                if let Some(commit) = &commit {
                    source["sha"] = json!(commit);
                }
                match (
                    &target.repo_id,
                    target.repo.as_deref().and_then(|repo| repo.split_once('/')),
                ) {
                    (Some(repo_id), _) => source["repoId"] = json!(repo_id),
                    (None, Some((org, name))) => {
                        source["org"] = json!(org);
                        source["repo"] = json!(name);
                    }
                    _ => {}
                }
                body["gitSource"] = source;
                if production {
                    body["target"] = json!("production");
                }
            } else {
                // 没关联 GitHub（或连的是别的 Git 平台）：只能把最近一次部署原样再来一遍
                let latest = list(state, Some(target.id), Some(1))?
                    .into_iter()
                    .next()
                    .ok_or(
                        "这个项目还没有部署过，也没有关联 GitHub 仓库：请先在 Vercel 上部署一次",
                    )?;
                if latest.branch.as_deref() != Some(branch.as_str()) {
                    return Err(format!(
                        "这个项目没有关联 GitHub 仓库，只能把最近一次部署（分支 {}）原样再来一遍",
                        latest.branch.as_deref().unwrap_or("？")
                    ));
                }
                body["deploymentId"] = json!(latest.id);
                body["withLatestCommit"] = json!(true);
            }
            let path = with_team(&target, "/v13/deployments");
            let v = vercel(state, &token, "POST", &path, Some(body)).await?;
            record(
                state,
                "deploy.trigger",
                "info",
                &target.name,
                &format!(
                    "发起了{}部署：{branch}",
                    env_label(if production { "production" } else { "preview" })
                ),
                &event_link(target.id),
            );
            text(&v, "id")
                .or_else(|| text(&v, "uid"))
                .ok_or_else(|| "Vercel 没有返回部署的编号".to_string())?
        }
        Provider::Pages => {
            let branch = branch.ok_or("这个项目没有记录生产分支，请指定要部署的分支")?;
            refresh_target(state, &target).await.ok();
            let last = list(state, Some(target.id), Some(200))?
                .into_iter()
                .find(|dep| dep.branch.as_deref() == Some(branch.as_str()))
                .ok_or_else(|| {
                    format!("分支「{branch}」还没有部署过：推送后 Cloudflare 会自动部署")
                })?;
            gate(last.env == "production")?;
            let v = cf(
                state,
                &token,
                "POST",
                &pages_path(&target, &format!("deployments/{}/retry", last.id)),
                None,
            )
            .await?;
            record(
                state,
                "deploy.trigger",
                "info",
                &target.name,
                &format!("发起了{}部署：{branch}", env_label(&last.env)),
                &event_link(target.id),
            );
            text(&v, "id").ok_or_else(|| "Cloudflare 没有返回新部署的编号".to_string())?
        }
        Provider::Workers => {
            // Hook 构建的是哪条分支由 Hook 决定，多半就是生产分支：一律按生产部署对待
            gate(true)?;
            fire_hook(state, &target).await?;
            record(
                state,
                "deploy.trigger",
                "info",
                &target.name,
                "触发了 Deploy Hook",
                &event_link(target.id),
            );
            refresh_target(state, &target).await?;
            return list(state, Some(target.id), Some(1))?
                .into_iter()
                .next()
                .ok_or_else(|| "已经触发，但还没看到新的构建".to_string());
        }
    };
    refresh_target(state, &target).await?;
    load_deployment(&state.db.lock().unwrap(), &id)
}

/// 把某一次部署原样再来一遍
pub async fn retry(state: &Arc<AppState>, id: &str, confirmed: bool) -> Result<Deployment, String> {
    let dep = load_deployment(&state.db.lock().unwrap(), id)?;
    let target = load_target(&state.db.lock().unwrap(), dep.target_id)?;
    let provider = Provider::parse(&target.provider)?;
    let token = token_of(state, provider)?;
    if dep.env == "production" && target.prod_requires_approval && !confirmed {
        return Err("这是生产部署，需要先确认".to_string());
    }
    let new_id = match provider {
        // 用分支上最新的提交重做，Vercel 会生成一次新的部署
        Provider::Vercel => {
            let body = json!({
                "name": target.name,
                "project": target.platform_id,
                "deploymentId": dep.id,
                "withLatestCommit": true,
            });
            let path = with_team(&target, "/v13/deployments");
            let v = vercel(state, &token, "POST", &path, Some(body)).await?;
            text(&v, "id").or_else(|| text(&v, "uid"))
        }
        Provider::Pages => {
            let v = cf(
                state,
                &token,
                "POST",
                &pages_path(&target, &format!("deployments/{}/retry", dep.id)),
                None,
            )
            .await?;
            text(&v, "id")
        }
        Provider::Workers => {
            fire_hook(state, &target).await?;
            None
        }
    };
    record(
        state,
        "deploy.retry",
        "info",
        &target.name,
        &format!(
            "重新部署 {}：{}",
            env_label(&dep.env),
            commit_line(dep.commit_sha.as_deref(), dep.commit_message.as_deref())
        ),
        &event_link(target.id),
    );
    let _ = refresh_target(state, &target).await;
    match new_id {
        Some(id) => load_deployment(&state.db.lock().unwrap(), &id),
        None => list(state, Some(target.id), Some(1))?
            .into_iter()
            .next()
            .ok_or_else(|| "已经触发，但还没看到新的构建".to_string()),
    }
}

/// 取消一次还在排队或构建中的部署
pub async fn cancel(state: &AppState, id: &str) -> Result<(), String> {
    let dep = load_deployment(&state.db.lock().unwrap(), id)?;
    let target = load_target(&state.db.lock().unwrap(), dep.target_id)?;
    let provider = Provider::parse(&target.provider)?;
    let token = token_of(state, provider)?;
    match provider {
        Provider::Vercel => {
            let path = with_team(&target, &format!("/v12/deployments/{}/cancel", dep.id));
            vercel(state, &token, "PATCH", &path, None).await?;
        }
        Provider::Workers => {
            cf(
                state,
                &token,
                "PUT",
                &format!("/accounts/{}/builds/builds/{}/cancel", target.scope, dep.id),
                None,
            )
            .await?;
        }
        Provider::Pages => {
            return Err("Cloudflare Pages 没有取消部署的接口：请到控制台里取消".to_string());
        }
    }
    record(
        state,
        "deploy.cancel",
        "warning",
        &target.name,
        &format!(
            "取消了{}部署：{}",
            env_label(&dep.env),
            commit_line(dep.commit_sha.as_deref(), dep.commit_message.as_deref())
        ),
        &event_link(target.id),
    );
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE deployments SET state = 'canceled', guarded = 1 WHERE id = ?1 AND state IN ('queued', 'building')",
            params![id],
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Vercel：把一次预览部署直接提升为生产，不重新构建
pub async fn promote(state: &AppState, id: &str) -> Result<(), String> {
    let dep = load_deployment(&state.db.lock().unwrap(), id)?;
    let target = load_target(&state.db.lock().unwrap(), dep.target_id)?;
    let provider = Provider::parse(&target.provider)?;
    if provider != Provider::Vercel {
        return Err("只有 Vercel 能把预览部署提升为生产；Cloudflare 请用回滚".to_string());
    }
    if dep.state != "ready" {
        return Err("只有成功的部署才能提升为生产".to_string());
    }
    let token = token_of(state, provider)?;
    let path = with_team(
        &target,
        &format!("/v10/projects/{}/promote/{}", target.platform_id, dep.id),
    );
    vercel(state, &token, "POST", &path, None).await?;
    record(
        state,
        "deploy.promote",
        "warning",
        &target.name,
        &format!(
            "把这次部署提升为生产：{}",
            commit_line(dep.commit_sha.as_deref(), dep.commit_message.as_deref())
        ),
        &event_link(target.id),
    );
    Ok(())
}

/// 回滚：生产流量切回之前的某一次部署。Workers 回滚的单位是版本，要给出 `version_id`
pub async fn rollback(
    state: &Arc<AppState>,
    target_id: i64,
    to_id: &str,
    version_id: Option<&str>,
) -> Result<(), String> {
    let target = load_target(&state.db.lock().unwrap(), target_id)?;
    let provider = Provider::parse(&target.provider)?;
    let token = token_of(state, provider)?;
    match provider {
        Provider::Vercel => {
            let path = with_team(
                &target,
                &format!(
                    "/v1/projects/{}/rollback/{}?description=AutoAgents+rollback",
                    target.platform_id, to_id
                ),
            );
            vercel(state, &token, "POST", &path, None).await?;
        }
        Provider::Pages => {
            cf(
                state,
                &token,
                "POST",
                &pages_path(&target, &format!("deployments/{to_id}/rollback")),
                None,
            )
            .await?;
        }
        Provider::Workers => {
            let version = version_id
                .map(str::trim)
                .filter(|version| !version.is_empty())
                .ok_or_else(|| "回滚 Worker 需要指定要回到哪个版本".to_string())?;
            cf(
                state,
                &token,
                "POST",
                &format!(
                    "/accounts/{}/workers/scripts/{}/deployments",
                    target.scope, target.name
                ),
                Some(json!({
                    "strategy": "percentage",
                    "versions": [{ "percentage": 100, "version_id": version }],
                    "annotations": { "workers/message": "AutoAgents 回滚" },
                })),
            )
            .await?;
        }
    }
    record(
        state,
        "deploy.rollback",
        "warning",
        &target.name,
        "生产部署已回滚",
        &event_link(target.id),
    );
    let _ = refresh_target(state, &target).await;
    Ok(())
}

/// 可以回滚到哪些部署
pub async fn history(state: &Arc<AppState>, target_id: i64) -> Result<Vec<HistoryItem>, String> {
    let target = load_target(&state.db.lock().unwrap(), target_id)?;
    if Provider::parse(&target.provider)? != Provider::Workers {
        let db = state.db.lock().unwrap();
        let query = |filter: &str| -> Result<Vec<Deployment>, String> {
            let mut stmt = db
                .prepare_cached(&format!(
                    "SELECT {DEP_COLS} FROM deployments WHERE target_id = ?1 AND env = 'production' \
                     AND state = 'ready' AND {filter} ORDER BY created_ms DESC LIMIT 50"
                ))
                .map_err(|e| e.to_string())?;
            stmt.query_map(params![target_id], row_to_deployment)
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())
        };
        // 平台给了「能回滚」的标记就只列那些；没给的话，成功的生产部署都列出来
        let rows = query("rollback_ok = 1")?;
        let rows = if rows.is_empty() {
            query("1 = 1")?
        } else {
            rows
        };
        return Ok(rows
            .into_iter()
            .map(|dep| HistoryItem {
                id: dep.id,
                created_ms: dep.created_ms,
                branch: dep.branch,
                commit_message: dep.commit_message,
                current: dep.current,
                version_id: None,
                health: dep.health,
            })
            .collect());
    }

    // Workers：回滚的单位是「版本」。部署记录指明用的是哪个版本，版本记录带着时间与说明
    let token = token_of(state, Provider::Workers)?;
    let base = format!("/accounts/{}/workers/scripts/{}", target.scope, target.name);
    let versions = cf(state, &token, "GET", &format!("{base}/versions"), None).await?;
    let mut info: HashMap<String, (Option<i64>, Option<String>)> = HashMap::new();
    for version in versions
        .as_array()
        .or_else(|| versions["items"].as_array())
        .into_iter()
        .flatten()
    {
        let Some(id) = text(version, "id") else {
            continue;
        };
        info.insert(
            id,
            (
                text(&version["metadata"], "created_on").and_then(|s| iso_ms(&s)),
                text(&version["annotations"], "workers/message"),
            ),
        );
    }
    let deployments = cf(state, &token, "GET", &format!("{base}/deployments"), None).await?;
    let mut items: Vec<HistoryItem> = deployments
        .as_array()
        .or_else(|| deployments["deployments"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|deployment| {
            let version_id = text(&deployment["versions"][0], "version_id")?;
            let (created_ms, message) = info.get(&version_id).cloned().unwrap_or((None, None));
            Some(HistoryItem {
                health: None,
                id: text(deployment, "id")?,
                created_ms: created_ms
                    .or_else(|| text(deployment, "created_on").and_then(|s| iso_ms(&s))),
                branch: None,
                commit_message: message
                    .or_else(|| text(&deployment["annotations"], "workers/message"))
                    .or_else(|| {
                        Some(format!(
                            "版本 …{}",
                            version_id
                                .chars()
                                .skip(version_id.chars().count().saturating_sub(8))
                                .collect::<String>()
                        ))
                    }),
                current: false,
                version_id: Some(version_id),
            })
        })
        .collect();
    items.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
    if let Some(first) = items.first_mut().filter(|item| item.created_ms.is_some()) {
        first.current = true;
    }
    Ok(items)
}

/// 构建日志，从后面截断
pub async fn logs(state: &AppState, dep: &Deployment) -> Result<String, String> {
    let target = load_target(&state.db.lock().unwrap(), dep.target_id)?;
    let provider = Provider::parse(&target.provider)?;
    let token = token_of(state, provider)?;
    let text = match provider {
        Provider::Vercel => {
            let path = with_team(
                &target,
                &format!("/v3/deployments/{}/events?limit=-1", dep.id),
            );
            let v = vercel(state, &token, "GET", &path, None).await?;
            v.as_array()
                .map(|events| {
                    events
                        .iter()
                        .filter_map(|event| text(&event["payload"], "text"))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        }
        Provider::Pages => {
            let v = cf(
                state,
                &token,
                "GET",
                &pages_path(&target, &format!("deployments/{}/history/logs", dep.id)),
                None,
            )
            .await?;
            v["data"]
                .as_array()
                .map(|lines| {
                    lines
                        .iter()
                        .filter_map(|line| text(line, "line"))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        }
        Provider::Workers => {
            // 日志是游标分页的，每条是 [时间戳, 内容]
            let path = format!("/accounts/{}/builds/builds/{}/logs", target.scope, dep.id);
            let mut lines = Vec::new();
            let mut cursor = String::new();
            for _ in 0..5 {
                let path = if cursor.is_empty() {
                    path.clone()
                } else {
                    format!("{path}?cursor={cursor}")
                };
                let v = cf(state, &token, "GET", &path, None).await?;
                for pair in v["lines"].as_array().into_iter().flatten() {
                    let Some([_, line, ..]) = pair.as_array().map(Vec::as_slice) else {
                        continue;
                    };
                    if let Some(line) = line.as_str() {
                        lines.push(line.to_string());
                    }
                }
                match text(&v, "cursor") {
                    Some(next) if next != cursor => cursor = next,
                    _ => break,
                }
            }
            lines.join("\n")
        }
    };
    Ok(clip_tail(&text, LOG_LIMIT))
}

/// 现在就查一次健康，结果记在这条部署上
pub async fn check_health(state: &AppState, id: &str) -> Result<String, String> {
    let dep = load_deployment(&state.db.lock().unwrap(), id)?;
    let target = load_target(&state.db.lock().unwrap(), dep.target_id)?;
    let verdict = match health_url(&target) {
        Some(url) => check(&url, target.health_contains.as_deref()).await,
        None => Health::Unknown("没有生产地址：在目标设置里填一个".to_string()),
    };
    let text = match &verdict {
        Health::Ok => "ok".to_string(),
        Health::Fail(text) | Health::Unknown(text) => text.clone(),
    };
    state
        .db
        .lock()
        .unwrap()
        .execute(
            "UPDATE deployments SET health = ?1 WHERE id = ?2",
            params![text, dep.id],
        )
        .map_err(|e| e.to_string())?;
    Ok(text)
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn deploy_status(state: State<'_, Arc<AppState>>) -> Result<DeployStatus, String> {
    Ok(DeployStatus {
        vercel: status(&state, "vercel").await,
        cloudflare: status(&state, "cloudflare").await,
    })
}

#[tauri::command]
pub async fn deploy_connect(
    state: State<'_, Arc<AppState>>,
    platform: String,
    token: String,
) -> Result<ProviderStatus, String> {
    connect(&state, &platform, &token).await
}

#[tauri::command]
pub async fn deploy_disconnect(
    state: State<'_, Arc<AppState>>,
    platform: String,
) -> Result<(), String> {
    disconnect(&state, &platform)
}

#[tauri::command]
pub async fn deploy_set_scope(
    state: State<'_, Arc<AppState>>,
    platform: String,
    scope: Option<String>,
) -> Result<(), String> {
    set_scope(&state, &platform, scope.as_deref())
}

#[tauri::command]
pub async fn deploy_discover(
    state: State<'_, Arc<AppState>>,
    platform: String,
) -> Result<Vec<Candidate>, String> {
    discover(&state, &platform).await
}

#[tauri::command]
pub fn deploy_targets(state: State<'_, Arc<AppState>>) -> Result<Vec<Target>, String> {
    targets(&state.db.lock().unwrap())
}

#[tauri::command]
pub async fn deploy_target_add(
    state: State<'_, Arc<AppState>>,
    provider: String,
    name: String,
    platform_id: String,
    url: Option<String>,
    hook_url: Option<String>,
) -> Result<Target, String> {
    add_target(
        &state,
        &provider,
        &name,
        &platform_id,
        url.as_deref(),
        hook_url.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn deploy_target_update(
    state: State<'_, Arc<AppState>>,
    id: i64,
    url: Option<String>,
    prod_requires_approval: Option<bool>,
    auto_fix: Option<bool>,
    auto_pr: Option<bool>,
    auto_deploy: Option<bool>,
    auto_rollback: Option<bool>,
    health_path: Option<String>,
    health_contains: Option<String>,
    hook_url: Option<String>,
) -> Result<Target, String> {
    update_target(
        &state,
        id,
        url.as_deref(),
        prod_requires_approval,
        auto_fix,
        auto_pr,
        auto_deploy,
        auto_rollback,
        health_path.as_deref(),
        health_contains.as_deref(),
        hook_url.as_deref(),
    )
}

#[tauri::command]
pub async fn deploy_target_remove(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    remove_target(&state, id)
}

#[tauri::command]
pub fn deploy_list(
    state: State<'_, Arc<AppState>>,
    target_id: Option<i64>,
    limit: Option<i64>,
) -> Result<Vec<Deployment>, String> {
    list(&state, target_id, limit)
}

#[tauri::command]
pub async fn deploy_refresh(
    state: State<'_, Arc<AppState>>,
    target_id: Option<i64>,
) -> Result<usize, String> {
    refresh(&state, target_id).await
}

#[tauri::command]
pub async fn deploy_trigger(
    state: State<'_, Arc<AppState>>,
    target_id: i64,
    branch: Option<String>,
    commit: Option<String>,
    confirmed: Option<bool>,
) -> Result<Deployment, String> {
    trigger(
        &state,
        target_id,
        branch.as_deref(),
        commit.as_deref(),
        confirmed.unwrap_or(false),
    )
    .await
}

#[tauri::command]
pub async fn deploy_retry(
    state: State<'_, Arc<AppState>>,
    id: String,
    confirmed: Option<bool>,
) -> Result<Deployment, String> {
    retry(&state, &id, confirmed.unwrap_or(false)).await
}

#[tauri::command]
pub async fn deploy_cancel(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    cancel(&state, &id).await
}

#[tauri::command]
pub async fn deploy_promote(state: State<'_, Arc<AppState>>, id: String) -> Result<(), String> {
    promote(&state, &id).await
}

#[tauri::command]
pub async fn deploy_rollback(
    state: State<'_, Arc<AppState>>,
    target_id: i64,
    to_id: String,
    version_id: Option<String>,
) -> Result<(), String> {
    rollback(&state, target_id, &to_id, version_id.as_deref()).await
}

#[tauri::command]
pub async fn deploy_history(
    state: State<'_, Arc<AppState>>,
    target_id: i64,
) -> Result<Vec<HistoryItem>, String> {
    history(&state, target_id).await
}

#[tauri::command]
pub async fn deploy_logs(state: State<'_, Arc<AppState>>, id: String) -> Result<String, String> {
    let dep = load_deployment(&state.db.lock().unwrap(), &id)?;
    logs(&state, &dep).await
}

#[tauri::command]
pub async fn deploy_check_health(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> Result<String, String> {
    check_health(&state, &id).await
}

#[tauri::command]
pub fn deploy_for_task(
    state: State<'_, Arc<AppState>>,
    task_id: i64,
) -> Result<Vec<Deployment>, String> {
    for_task(&state, task_id)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use crate::testutil::{
        Req, fake_agent, fake_script, read_request, respond, service_for, setup_repo_at, test_state,
    };
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicI64, Ordering};

    type Calls = Arc<Mutex<Vec<(String, String, String)>>>;

    /// 假的 Vercel / Cloudflare / GitHub，外加几个冒充生产地址的站点
    struct Fake {
        addr: String,
        /// (方法, 路径带查询串, 请求体)
        calls: Calls,
        vercel: Arc<Mutex<Vec<Value>>>,
        pages: Arc<Mutex<Vec<Value>>>,
        workers: Arc<Mutex<Vec<Value>>>,
        /// GET /repos/*/pulls/* 的应答（PR 详情）
        pulls: Arc<Mutex<Value>>,
        seq: Arc<AtomicI64>,
    }

    impl Fake {
        /// 冒充生产地址的站点：`/healthz` 正常，`/down` 挂了，`/private` 有访问保护
        fn site(&self, path: &str) -> String {
            format!("{addr}/healthz{path}", addr = self.addr)
                .replace("/healthz/down", "/down")
                .replace("/healthz/private", "/private")
        }

        /// 最近一次这样的请求：(路径带查询串, 请求体)
        fn call(&self, method: &str, part: &str) -> Option<(String, String)> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(m, path, _)| m == method && path.contains(part))
                .map(|(_, path, body)| (path.clone(), body.clone()))
        }

        /// 最近一次这样的请求的请求体
        fn body(&self, method: &str, part: &str) -> Value {
            serde_json::from_str(&self.call(method, part).unwrap().1).unwrap()
        }
    }

    fn projects() -> Value {
        json!({"projects": [
            {"id": "prj_web", "name": "web", "framework": "nextjs",
             "link": {"type": "github", "org": "octo", "repo": "demo", "repoId": 1234, "productionBranch": "main"}},
            {"id": "prj_alt", "name": "alt", "framework": null,
             "link": {"type": "github", "org": "octo", "repo": "demo", "repoId": 1234, "productionBranch": "release"}},
            {"id": "prj_u", "name": "u", "framework": null,
             "link": {"type": "github", "org": "octo", "repo": "demo", "repoId": 1234, "productionBranch": "main"}},
            {"id": "prj_off", "name": "off", "framework": null,
             "link": {"type": "github", "org": "octo", "repo": "demo", "repoId": 1234, "productionBranch": "main"}},
            {"id": "prj_plain", "name": "plain", "framework": null, "link": {}}
        ]})
    }

    fn pages_project() -> Value {
        json!([{
            "id": "pgp1", "name": "site", "subdomain": "acme.pages.dev", "domains": ["site.example.com"],
            "production_branch": "main", "framework": "nextjs",
            "source": {"type": "github", "config": {"owner": "octo", "repo_name": "demo", "repo_id": 1234, "production_branch": "main"}}
        }])
    }

    fn worker_deployments() -> Value {
        json!([
            {"id": "wdep2", "created_on": "2026-11-23T00:01:00Z", "strategy": "percentage", "versions": [{"percentage": 100, "version_id": "ver2"}]},
            {"id": "wdep1", "created_on": "2026-11-20T00:01:00Z", "strategy": "percentage", "versions": [{"percentage": 100, "version_id": "ver1"}]}
        ])
    }

    fn worker_versions() -> Value {
        json!([
            {"id": "ver1", "number": 1, "metadata": {"created_on": "2026-11-20T00:00:00Z"}, "annotations": {"workers/message": "add health endpoint"}},
            {"id": "ver2", "number": 2, "metadata": {"created_on": "2026-11-23T00:00:00Z"}, "annotations": {"workers/message": "bump deps"}}
        ])
    }

    fn set(list: &Mutex<Vec<Value>>, id: &str, key: &str, value: &str) {
        for item in list.lock().unwrap().iter_mut() {
            if item["uid"] == json!(id) || item["build_uuid"] == json!(id) {
                item[key] = json!(value);
            }
        }
    }

    /// 假平台对一次请求的回应：(状态行, Content-Type, 正文)
    fn route(
        req: &Req,
        vercel: &Mutex<Vec<Value>>,
        pages: &Mutex<Vec<Value>>,
        workers: &Mutex<Vec<Value>>,
        pulls: &Mutex<Value>,
        seq: &AtomicI64,
    ) -> (String, &'static str, String) {
        let reply = |status: &'static str, value: &Value| {
            (status.to_string(), "application/json", value.to_string())
        };
        // GitHub：PR 详情（merged 与否、目标分支、合并提交）
        if req.method == "GET" && req.path.starts_with("/repos/") && req.path.contains("/pulls/") {
            let detail = pulls.lock().unwrap().clone();
            if detail.is_null() {
                return reply("404 Not Found", &json!({"message": "not found"}));
            }
            return reply("200 OK", &detail);
        }
        let envelope = |result: Value| {
            reply(
                "200 OK",
                &json!({"success": true, "errors": [], "messages": [], "result": result}),
            )
        };
        let (path, query) = req.path.split_once('?').unwrap_or((req.path.as_str(), ""));
        let parts: Vec<&str> = path
            .trim_matches('/')
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        let body: Value = serde_json::from_str(&req.body).unwrap_or(Value::Null);
        let bearer = req
            .headers
            .get("authorization")
            .map(String::as_str)
            .unwrap_or("");

        // 冒充生产地址的站点
        match parts.first().copied() {
            Some("healthz") => {
                return ("200 OK".into(), "text/html", "<html>all good</html>".into());
            }
            Some("down") => {
                return (
                    "500 Internal Server Error".into(),
                    "text/plain",
                    "boom".into(),
                );
            }
            Some("private") => return ("401 Unauthorized".into(), "text/plain", String::new()),
            _ => {}
        }
        // Deploy Hook：地址本身就是凭据，带了 Authorization 反而不认
        if let ["workers", "builds", "deploy_hooks", _] = parts.as_slice() {
            if bearer.contains("Bearer") {
                return ("401 Unauthorized".into(), "application/json", "{}".into());
            }
            let build = json!({"build_uuid": "wk_new", "projectId": "wk", "status": "queued",
                               "created_on": "2026-11-24T12:00:00Z", "build_trigger_metadata": {"branch": "main", "commit_hash": "eeeeee5555"}});
            workers.lock().unwrap().insert(0, build.clone());
            return envelope(
                json!({"build_uuid": "wk_new", "branch": "main", "worker": "api-worker"}),
            );
        }
        if parts.first() == Some(&"repos") {
            return reply(
                "200 OK",
                &json!({"default_branch": "main", "full_name": "octo/demo"}),
            );
        }
        let is_vercel = parts.first().is_some_and(|part| part.starts_with('v'));
        if is_vercel && bearer != "Bearer vcp_good" {
            return reply(
                "403 Forbidden",
                &json!({"error": {"code": "forbidden", "message": "Not authorized", "invalidToken": true}}),
            );
        }
        if parts.first() == Some(&"accounts") && bearer != "Bearer cf_good" {
            return reply(
                "400 Bad Request",
                &json!({"success": false, "errors": [{"code": 10000, "message": "Invalid API Token"}]}),
            );
        }

        let vercel_list = vercel.lock().unwrap().clone();
        match (req.method.as_str(), parts.as_slice()) {
            ("GET", ["v10", "projects"]) => reply("200 OK", &projects()),
            ("GET", ["v9", "projects", id]) => projects()["projects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|project| project["id"] == json!(id))
                .map(|project| reply("200 OK", project))
                .unwrap_or_else(|| reply("404 Not Found", &json!({"error": {"code": "not_found", "message": "The project was not found"}}))),
            ("GET", ["v2", "user"]) => reply("200 OK", &json!({"user": {"username": "octo", "email": "octo@example.com", "defaultTeamId": "team_a"}})),
            ("GET", ["v2", "teams"]) => reply("200 OK", &json!({"teams": [{"id": "team_a", "slug": "acme", "name": "Acme"}]})),
            ("GET", ["v7", "deployments"]) => {
                let project = query.split('&').find_map(|kv| kv.strip_prefix("projectId=")).unwrap_or("");
                let mine: Vec<Value> = vercel_list.iter().filter(|dep| dep["projectId"] == json!(project)).cloned().collect();
                reply("200 OK", &json!({"deployments": mine}))
            }
            ("GET", ["v13", "deployments", id]) => vercel_list
                .iter()
                .find(|dep| dep["uid"] == json!(id))
                .map(|dep| {
                    let mut full = dep.clone();
                    full["alias"] = json!(["127.0.0.1", "web-vercel.vercel.app"]);
                    reply("200 OK", &full)
                })
                .unwrap_or_else(|| reply("404 Not Found", &json!({"error": {"code": "not_found", "message": "The deployment was not found"}}))),
            ("POST", ["v13", "deployments"]) => {
                let n = seq.fetch_add(1, Ordering::SeqCst) + 1;
                let mut dep = json!({
                    "uid": format!("dpl_new{n}"), "projectId": body["project"], "name": body["name"],
                    "url": format!("web-{n}.vercel.app"), "readyState": "QUEUED", "created": now_ms(),
                    "meta": {"githubCommitRef": body["gitSource"]["ref"], "githubCommitSha": body["gitSource"]["sha"], "githubCommitMessage": "triggered"}
                });
                if body["target"] == json!("production") {
                    dep["target"] = json!("production");
                }
                vercel.lock().unwrap().insert(0, dep.clone());
                reply("201 Created", &dep)
            }
            ("PATCH", ["v12", "deployments", id, "cancel"]) => {
                set(vercel, id, "readyState", "CANCELED");
                reply("200 OK", &json!({"id": id, "readyState": "CANCELED"}))
            }
            ("GET", ["v3", "deployments", _id, "events"]) => reply("200 OK", &json!([
                {"type": "stdout", "created": 1, "payload": {"text": "Installing dependencies…"}},
                {"type": "delimiter", "created": 2, "payload": {"text": ""}},
                {"type": "stderr", "created": 3, "payload": {"text": "Type error: Cannot find name 'useEffect'", "level": "error"}}
            ])),
            ("POST", ["v10", "projects", _project, "promote", dep]) | ("POST", ["v1", "projects", _project, "rollback", dep]) => {
                if !vercel_list.iter().any(|item| item["uid"] == json!(dep)) {
                    return reply("404 Not Found", &json!({"error": {"code": "not_found", "message": "The deployment was not found"}}));
                }
                // 生产流量切到这次部署上：它成为当前版本，其余成功的生产部署退回去
                for item in vercel.lock().unwrap().iter_mut() {
                    if item["uid"] == json!(dep) {
                        item["target"] = json!("production");
                        item["readyState"] = json!("READY");
                        item["readySubstate"] = json!("PROMOTED");
                    } else if item["readyState"] == json!("READY") && item["target"] == json!("production") {
                        item["readySubstate"] = json!("STAGED");
                    }
                }
                reply("201 Created", &json!({}))
            }

            ("GET", ["accounts"]) => envelope(json!([{"id": "acct1", "name": "Acme"}])),
            ("GET", ["accounts", _, "pages", "projects"]) => envelope(pages_project()),
            ("GET", ["accounts", _, "pages", "projects", name]) => pages_project()
                .as_array()
                .unwrap()
                .iter()
                .find(|project| project["name"] == json!(name))
                .map(|project| envelope(project.clone()))
                .unwrap_or_else(|| reply("404 Not Found", &json!({"success": false, "errors": [{"code": 7003, "message": "No project"}]}))),
            ("GET", ["accounts", _, "pages", "projects", _, "deployments"]) => envelope(json!(*pages.lock().unwrap())),
            ("POST", ["accounts", _, "pages", "projects", _, "deployments", id, "retry"]) => {
                let n = seq.fetch_add(1, Ordering::SeqCst) + 1;
                let mut dep = pages.lock().unwrap().iter().find(|dep| dep["id"] == json!(id)).cloned().unwrap_or_else(|| json!({}));
                dep["id"] = json!(format!("pg_new{n}"));
                dep["url"] = json!(format!("new-{n}.site.pages.dev"));
                dep["latest_stage"] = json!({"name": "queued", "status": "idle"});
                dep["is_skipped"] = json!(false);
                pages.lock().unwrap().insert(0, dep.clone());
                envelope(dep)
            }
            ("POST", ["accounts", _, "pages", "projects", _, "deployments", _, "rollback"]) => envelope(json!({})),
            ("GET", ["accounts", _, "pages", "projects", _, "deployments", _, "history", "logs"]) => envelope(json!({
                "data": [{"line": "Cloning repository…", "ts": "2026-11-23T10:00:01Z"}, {"line": "✘ [ERROR] Build failed with 1 error", "ts": "2026-11-23T10:00:02Z"}],
                "total": 2, "includes_container_logs": false
            })),
            ("GET", ["accounts", _, "workers", "scripts"]) => envelope(json!([{"id": "api-worker", "tag": "tag_api_worker"}])),
            ("GET", ["accounts", _, "workers", "subdomain"]) => envelope(json!({"subdomain": "acme"})),
            ("GET", ["accounts", _, "builds", "workers", _tag]) => envelope(json!({
                "git_repository": {"repo_name": "octo/demo", "branch": "main", "provider": "github"},
                "production_settings": {"build_command": "npm run build"}, "previews_enabled": true
            })),
            ("GET", ["accounts", _, "builds", "workers", _tag, "builds"]) => envelope(json!(*workers.lock().unwrap())),
            ("GET", ["accounts", _, "builds", "builds", _uuid, "logs"]) => envelope(json!({
                "lines": [[1795428000000i64, "Running build command"], [1795428001000i64, "✘ [ERROR] build failed"]], "truncated": false
            })),
            ("PUT", ["accounts", _, "builds", "builds", uuid, "cancel"]) => {
                set(workers, uuid, "status", "canceled");
                envelope(json!({}))
            }
            ("GET", ["accounts", _, "workers", "scripts", _, "deployments"]) => envelope(json!({"deployments": worker_deployments()})),
            ("POST", ["accounts", _, "workers", "scripts", _, "deployments"]) => envelope(json!({"id": format!("wdep_after_{}", body["versions"][0]["version_id"].as_str().unwrap_or("x"))})),
            ("GET", ["accounts", _, "workers", "scripts", _, "versions"]) => envelope(worker_versions()),
            _ if is_vercel => reply("404 Not Found", &json!({"error": {"code": "not_found", "message": "No route"}})),
            _ => reply("404 Not Found", &json!({"success": false, "errors": [{"code": 7000, "message": "No route for URI"}]})),
        }
    }

    async fn start_fake() -> Fake {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let pulls = Arc::new(Mutex::new(Value::Null));
        let fake = Fake {
            addr,
            calls: Arc::default(),
            pulls,
            vercel: Arc::default(),
            pages: Arc::default(),
            workers: Arc::default(),
            seq: Arc::new(AtomicI64::new(0)),
        };
        let (calls, vercel, pages, workers, pulls, seq) = (
            Arc::clone(&fake.calls),
            Arc::clone(&fake.vercel),
            Arc::clone(&fake.pages),
            Arc::clone(&fake.workers),
            Arc::clone(&fake.pulls),
            Arc::clone(&fake.seq),
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let (calls, vercel, pages, workers, pulls, seq) = (
                    Arc::clone(&calls),
                    Arc::clone(&vercel),
                    Arc::clone(&pages),
                    Arc::clone(&workers),
                    Arc::clone(&pulls),
                    Arc::clone(&seq),
                );
                tokio::spawn(async move {
                    let req = read_request(&mut stream).await;
                    calls.lock().unwrap().push((
                        req.method.clone(),
                        req.path.clone(),
                        req.body.clone(),
                    ));
                    let (status, kind, body) = route(&req, &vercel, &pages, &workers, &pulls, &seq);
                    respond(&mut stream, &status, kind, &body).await;
                });
            }
        });
        fake
    }

    /// 灌入一批部署。时间都用当前时刻往前推，守护里的「24 小时内」才数得到
    fn seed(fake: &Fake) {
        let now = now_ms();
        *fake.vercel.lock().unwrap() = vec![
            json!({"uid": "dpl_err", "projectId": "prj_web", "name": "web", "url": "web-err.vercel.app", "readyState": "ERROR", "target": "production",
                   "created": now - 500_000, "buildingAt": now - 490_000, "ready": now - 400_000, "errorCode": "BUILD_FAILED",
                   "errorMessage": "Command \"npm run build\" exited with 1", "inspectorUrl": "https://vercel.com/acme/web/err",
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "1111111222", "githubCommitMessage": "break the build", "githubCommitAuthorName": "Octo"}}),
            json!({"uid": "dpl_ok", "projectId": "prj_web", "name": "web", "url": "web-ok.vercel.app", "readyState": "READY", "readySubstate": "PROMOTED", "target": "production",
                   "created": now - 900_000, "ready": now - 800_000, "isRollbackCandidate": true, "inspectorUrl": "https://vercel.com/acme/web/ok",
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "abcdef123456", "githubCommitMessage": "ship it"}}),
            json!({"uid": "dpl_prev", "projectId": "prj_web", "name": "web", "url": "web-prev.vercel.app", "readyState": "READY", "readySubstate": "STAGED", "target": "production",
                   "created": now - 1_900_000, "ready": now - 1_800_000, "isRollbackCandidate": true,
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "aaaaaa0000", "githubCommitMessage": "previous release"}}),
            json!({"uid": "dpl_old", "projectId": "prj_web", "name": "web", "url": "web-old.vercel.app", "readyState": "READY", "readySubstate": "STAGED", "target": "production",
                   "created": now - 2_900_000, "ready": now - 2_800_000, "isRollbackCandidate": false,
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "0000000bbb", "githubCommitMessage": "too old to roll back to"}}),
            json!({"uid": "dpl_preview", "projectId": "prj_web", "name": "web", "url": "web-preview.vercel.app", "readyState": "READY", "target": null,
                   "created": now - 95_000, "ready": now - 90_000,
                   "meta": {"githubCommitRef": "feature/x", "githubCommitSha": "fffff0000", "githubCommitMessage": "work in progress"}}),
            json!({"uid": "dpl_build", "projectId": "prj_web", "name": "web", "url": null, "readyState": "BUILDING", "target": null, "created": now - 30_000, "buildingAt": now - 20_000,
                   "meta": {"githubCommitRef": "feature/x", "githubCommitSha": "fffff0000", "githubCommitMessage": "work in progress"}}),
            json!({"uid": "dpl_alt_ok", "projectId": "prj_alt", "name": "alt", "url": "alt.vercel.app", "readyState": "READY", "readySubstate": "PROMOTED", "target": "production",
                   "created": now - 700_000, "ready": now - 600_000,
                   "meta": {"githubCommitRef": "release", "githubCommitSha": "2222222ccc", "githubCommitMessage": "alt is up"}}),
            json!({"uid": "dpl_u_ok", "projectId": "prj_u", "name": "u", "url": "u.vercel.app", "readyState": "READY", "readySubstate": "PROMOTED", "target": "production",
                   "created": now - 700_000, "ready": now - 600_000,
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "3333333ddd", "githubCommitMessage": "u is up"}}),
            json!({"uid": "dpl_off_err", "projectId": "prj_off", "name": "off", "url": "off.vercel.app", "readyState": "ERROR", "target": "production",
                   "created": now - 500_000, "errorCode": "BUILD_FAILED", "errorMessage": "Command \"npm run build\" exited with 1",
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "4444444eee", "githubCommitMessage": "off broke"}}),
        ];
        *fake.pages.lock().unwrap() = vec![
            json!({"id": "pg_ok", "url": "site-ok.pages.dev", "environment": "production", "is_skipped": false, "created_on": "2026-11-23T10:00:00.123Z",
                   "latest_stage": {"name": "deploy", "status": "success"},
                   "stages": [{"name": "build", "status": "success", "ended_on": "2026-11-23T10:01:30Z"}, {"name": "deploy", "status": "success", "ended_on": "2026-11-23T10:02:00Z"}],
                   "aliases": ["site.example.com"],
                   "deployment_trigger": {"type": "github:push", "metadata": {"branch": "main", "commit_hash": "aaaaaa1111", "commit_message": "ship pages", "commit_dirty": false}}}),
            json!({"id": "pg_fail", "url": "site-fail.pages.dev", "environment": "preview", "is_skipped": false, "created_on": "2026-11-23T09:00:00Z",
                   "latest_stage": {"name": "build", "status": "failure"},
                   "deployment_trigger": {"type": "github:push", "metadata": {"branch": "feature/y", "commit_hash": "bbbbbb2222", "commit_message": "try things"}}}),
            json!({"id": "pg_skipped", "url": "site-skipped.pages.dev", "environment": "preview", "is_skipped": true, "created_on": "2026-11-23T08:00:00Z",
                   "latest_stage": {"name": "build", "status": "idle"},
                   "deployment_trigger": {"type": "github:push", "metadata": {"branch": "feature/y", "commit_hash": "cccccc3333", "commit_message": "docs only"}}}),
        ];
        *fake.workers.lock().unwrap() = vec![
            json!({"build_uuid": "wk_ok", "status": "success", "build_outcome": "success", "created_on": "2026-11-23T10:00:00Z",
                   "build_start_on": "2026-11-23T10:00:05Z", "build_end_on": "2026-11-23T10:01:00Z",
                   "build_trigger_metadata": {"branch": "main", "commit_hash": "cccccc3333", "commit_message": "bump"}}),
            json!({"build_uuid": "wk_fail", "status": "failure", "build_outcome": "failure", "created_on": "2026-11-23T09:00:00Z",
                   "build_trigger_metadata": {"branch": "feature/z", "commit_hash": "dddddd4444", "commit_message": "wip"}, "build_error": "Build command failed"}),
            json!({"build_uuid": "wk_run", "status": "running", "created_on": "2026-11-23T11:00:00Z", "build_trigger_metadata": {"branch": "main"}}),
        ];
    }

    /// 起假平台、连上两家（不灌部署）
    async fn rigged(service: Arc<autoagents_core::service::Service>) -> (Fake, Arc<AppState>) {
        let fake = start_fake().await;
        let ts = test_state(service);
        *ts.state.vercel_api.lock().unwrap() = fake.addr.clone();
        *ts.state.cf_api.lock().unwrap() = fake.addr.clone();
        *ts.state.gh_api.lock().unwrap() = fake.addr.clone();
        connect(&ts.state, "vercel", "vcp_good").await.unwrap();
        connect(&ts.state, "cloudflare", "cf_good").await.unwrap();
        (fake, ts.state)
    }

    /// 一个用不上的机器侧服务：这些用例里 Agent 不出场
    fn idle() -> Arc<autoagents_core::service::Service> {
        service_for(fake_script("claude", "true"), None)
    }

    /// (类别, 级别, 文案)，按时间先后
    fn events_of(state: &AppState) -> Vec<(String, String, String)> {
        let db = state.db.lock().unwrap();
        let mut stmt = db
            .prepare("SELECT kind, level, message FROM events ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// 等任务进入这些状态之一
    async fn wait_status(state: &AppState, task_id: i64, want: &[&str]) -> tasks::Task {
        for _ in 0..300 {
            let task = tasks::load(&state.db.lock().unwrap(), task_id).unwrap();
            if want.contains(&task.status.as_str()) {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "任务没有进入 {want:?}：{}",
            tasks::load(&state.db.lock().unwrap(), task_id)
                .unwrap()
                .status
        );
    }

    /// 冒充 Agent：写一个文件，然后报告做完了
    const AGENT: &str = r#"echo fixed > fix.txt
printf '%s\n' '{"type":"result","is_error":false,"result":"修好了构建。"}'"#;

    #[tokio::test]
    async fn tokens_are_verified_and_projects_can_be_registered() {
        let fake = start_fake().await;
        let ts = test_state(idle());
        *ts.state.vercel_api.lock().unwrap() = fake.addr.clone();
        *ts.state.cf_api.lock().unwrap() = fake.addr.clone();
        let state = ts.state.clone();

        // 令牌不对：不保存
        let err = connect(&state, "vercel", "vcp_bad").await.unwrap_err();
        assert!(err.contains("令牌无效"), "{err}");
        assert!(ts.secrets.get("deploy_vercel_token").unwrap().is_none());
        let err = connect(&state, "cloudflare", "nope").await.unwrap_err();
        assert!(err.contains("令牌无效"), "{err}");
        assert!(
            connect(&state, "gitlab", "x")
                .await
                .unwrap_err()
                .contains("不认识")
        );
        assert!(
            connect(&state, "vercel", "   ")
                .await
                .unwrap_err()
                .contains("请输入")
        );

        // 通过了才存进凭据库，并带出用户与团队 / 账户
        let vercel = connect(&state, "vercel", "vcp_good").await.unwrap();
        assert_eq!(vercel.who.as_deref(), Some("octo"));
        assert_eq!(vercel.scopes[0].name, "Acme");
        assert_eq!(vercel.scope.as_deref(), Some("team_a"));
        assert_eq!(
            ts.secrets.get("deploy_vercel_token").unwrap().as_deref(),
            Some("vcp_good")
        );
        let cloudflare = connect(&state, "cloudflare", "cf_good").await.unwrap();
        assert_eq!(cloudflare.who, None);
        assert_eq!(
            (
                cloudflare.scopes[0].id.as_str(),
                cloudflare.scope.as_deref()
            ),
            ("acct1", Some("acct1"))
        );
        assert!(status(&state, "cloudflare").await.error.is_none());

        // 还没登记的项目
        let found = discover(&state, "vercel").await.unwrap();
        assert_eq!(
            found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["alt", "off", "plain", "u", "web"]
        );
        let web = &found[4];
        assert_eq!(
            (
                web.repo.as_deref(),
                web.repo_id.as_deref(),
                web.branch.as_deref(),
                web.framework.as_deref()
            ),
            (
                Some("octo/demo"),
                Some("1234"),
                Some("main"),
                Some("nextjs")
            )
        );
        assert!(found.iter().all(|candidate| candidate.registered.is_none()));
        assert_eq!(
            fake.call("GET", "/v10/projects").unwrap().0,
            "/v10/projects?limit=100&teamId=team_a"
        );

        let found = discover(&state, "cloudflare").await.unwrap();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(
            (found[0].provider.as_str(), found[0].name.as_str()),
            ("cloudflare-pages", "site")
        );
        assert_eq!(found[0].url.as_deref(), Some("https://site.example.com"));
        assert_eq!(
            (found[0].repo.as_deref(), found[0].branch.as_deref()),
            (Some("octo/demo"), Some("main"))
        );
        assert_eq!(
            (found[1].provider.as_str(), found[1].platform_id.as_str()),
            ("cloudflare-workers", "tag_api_worker")
        );

        // 登记 Vercel 项目：仓库、分支从平台补全；范围是选中的团队
        let web = add_target(
            &state,
            "vercel",
            "web",
            "prj_web",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            (
                web.repo.as_deref(),
                web.repo_id.as_deref(),
                web.branch.as_deref(),
                web.git_type.as_deref(),
                web.framework.as_deref()
            ),
            (
                Some("octo/demo"),
                Some("1234"),
                Some("main"),
                Some("github"),
                Some("nextjs")
            )
        );
        assert_eq!(web.scope, "team_a");
        assert!(web.prod_requires_approval && web.auto_fix && !web.has_hook);
        assert_eq!(
            (web.health_path.as_str(), web.url.as_deref()),
            ("/", Some(fake.site("").as_str()))
        );
        // 同一个项目再登记一次：还是原来那个
        assert_eq!(
            add_target(&state, "vercel", "web", "prj_web", None, None)
                .await
                .unwrap()
                .id,
            web.id
        );
        assert_eq!(targets(&state.db.lock().unwrap()).unwrap().len(), 1);
        assert!(
            add_target(&state, "docker", "x", "y", None, None)
                .await
                .is_err()
        );
        assert!(
            add_target(&state, "vercel", "  ", "prj_web", None, None)
                .await
                .is_err()
        );

        // 登记 Worker：标签是平台给的；Deploy Hook 的地址进凭据库，不进数据库
        let hook =
            "https://hooks.cloudflare.example/workers/builds/deploy_hooks/hook123".to_string();
        let worker = add_target(
            &state,
            "cloudflare-workers",
            "api-worker",
            "tag_api_worker",
            None,
            Some(&hook),
        )
        .await
        .unwrap();
        assert!(worker.has_hook);
        assert_eq!(
            ts.secrets.get(&hook_account(worker.id)).unwrap().as_deref(),
            Some(hook.as_str())
        );
        assert_eq!(
            worker.url.as_deref(),
            Some("https://api-worker.acme.workers.dev")
        );
        assert_eq!(
            (
                worker.repo.as_deref(),
                worker.branch.as_deref(),
                worker.git_type.as_deref(),
                worker.scope.as_str()
            ),
            (Some("octo/demo"), Some("main"), Some("github"), "acct1")
        );
        assert!(
            serde_json::to_string(&worker)
                .unwrap()
                .find("deploy_hooks")
                .is_none(),
            "Hook 的地址不能跟着目标发给界面"
        );

        let site = add_target(
            &state,
            "cloudflare-pages",
            "site",
            "site",
            Some(&fake.site("/v2")),
            None,
        )
        .await
        .unwrap();
        assert_eq!(site.url.as_deref(), Some(fake.site("/v2").as_str()));
        assert_eq!(
            (site.repo.as_deref(), site.branch.as_deref()),
            (Some("octo/demo"), Some("main"))
        );

        // 列表里标出已经登记的
        let found = discover(&state, "cloudflare").await.unwrap();
        assert!(found.iter().all(|candidate| candidate.registered.is_some()));

        // 断开之后登记不了；重新连上就好了
        disconnect(&state, "cloudflare").unwrap();
        assert!(ts.secrets.get("deploy_cloudflare_token").unwrap().is_none());
        assert!(
            add_target(&state, "cloudflare-pages", "site2", "site2", None, None)
                .await
                .unwrap_err()
                .contains("尚未连接")
        );
        assert!(
            discover(&state, "cloudflare")
                .await
                .unwrap_err()
                .contains("尚未连接")
        );
        connect(&state, "cloudflare", "cf_good").await.unwrap();

        // 改设置：传了哪项改哪项
        let updated = update_target(
            &state,
            web.id,
            Some("demo.example.com/api"),
            Some(false),
            Some(false),
            Some(true),
            Some(true),
            Some(true),
            Some("health"),
            Some("  pong "),
            None,
        )
        .unwrap();
        assert_eq!(updated.url.as_deref(), Some("https://demo.example.com/api"));
        assert!(!updated.prod_requires_approval && !updated.auto_fix);
        assert!(updated.auto_pr && updated.auto_deploy && updated.auto_rollback);
        assert_eq!(
            (
                updated.health_path.as_str(),
                updated.health_contains.as_deref()
            ),
            ("/health", Some("pong"))
        );
        assert_eq!(
            health_url(&updated).as_deref(),
            Some("https://demo.example.com/api/health")
        );
        assert!(
            update_target(
                &state,
                web.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("http://insecure/hook")
            )
            .is_err()
        );
        assert!(
            !update_target(
                &state,
                worker.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("")
            )
            .unwrap()
            .has_hook,
            "清空 Hook"
        );
        assert!(ts.secrets.get(&hook_account(worker.id)).unwrap().is_none());
        assert_eq!(
            update_target(
                &state,
                web.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some(""),
                None
            )
            .unwrap()
            .health_contains,
            None
        );
        update_target(
            &state,
            web.id,
            Some(&fake.site("")),
            Some(true),
            Some(true),
            None,
            None,
            None,
            Some("/"),
            None,
            None,
        )
        .unwrap();

        // 取消登记：部署记录一并删掉，平台上的项目不动
        let plain = add_target(
            &state,
            "vercel",
            "plain",
            "prj_plain",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        assert!(plain.repo.is_none());
        remove_target(&state, plain.id).unwrap();
        assert!(load_target(&state.db.lock().unwrap(), plain.id).is_err());
        assert_eq!(targets(&state.db.lock().unwrap()).unwrap().len(), 3);
        assert!(
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .all(|(method, _, _)| method != "DELETE")
        );

        // 换团队：之后列项目按新的团队来
        set_scope(&state, "vercel", Some("team_b")).unwrap();
        discover(&state, "vercel").await.unwrap();
        assert!(
            fake.call("GET", "/v10/projects")
                .unwrap()
                .0
                .ends_with("teamId=team_b")
        );
        assert!(set_scope(&state, "github", Some("x")).is_err());

        let events = events_of(&state);
        for (kind, needle) in [
            ("deploy.connected", "Vercel 已连接（octo）"),
            ("deploy.target", "已登记部署目标（Cloudflare Workers）"),
            ("deploy.disconnected", "Cloudflare 已断开连接"),
        ] {
            assert!(
                events.iter().any(|(k, level, message)| k == kind
                    && level == "info"
                    && message.contains(needle)),
                "缺事件 {kind}：{needle}"
            );
        }
    }

    #[tokio::test]
    async fn deployments_from_all_three_platforms_are_normalized() {
        let (fake, state) = rigged(idle()).await;
        seed(&fake);
        let web = add_target(
            &state,
            "vercel",
            "web",
            "prj_web",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        let site = add_target(
            &state,
            "cloudflare-pages",
            "site",
            "site",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        let worker = add_target(
            &state,
            "cloudflare-workers",
            "api-worker",
            "tag_api_worker",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        // 登记时各拉过一次，这次什么都不该变
        assert_eq!(refresh(&state, None).await.unwrap(), 0);

        let deps = list(&state, Some(web.id), None).unwrap();
        let by = |id: &str| {
            deps.iter()
                .find(|dep| dep.id == id)
                .cloned()
                .unwrap_or_else(|| panic!("缺 {id}"))
        };
        let (err, ok, prev, old, preview, build) = (
            by("dpl_err"),
            by("dpl_ok"),
            by("dpl_prev"),
            by("dpl_old"),
            by("dpl_preview"),
            by("dpl_build"),
        );
        assert_eq!(deps.len(), 6);
        // 新的在前
        let order: Vec<&str> = deps.iter().map(|dep| dep.id.as_str()).collect();
        assert_eq!(
            order,
            [
                "dpl_build",
                "dpl_preview",
                "dpl_err",
                "dpl_ok",
                "dpl_prev",
                "dpl_old"
            ]
        );

        assert_eq!(
            (
                err.state.as_str(),
                err.env.as_str(),
                err.raw_state.as_deref()
            ),
            ("error", "production", Some("ERROR"))
        );
        assert_eq!(
            (
                err.branch.as_deref(),
                err.commit_sha.as_deref(),
                err.commit_message.as_deref(),
                err.author.as_deref()
            ),
            (
                Some("main"),
                Some("1111111222"),
                Some("break the build"),
                Some("Octo")
            )
        );
        assert_eq!(
            (err.error_code.as_deref(), err.error_message.as_deref()),
            (
                Some("BUILD_FAILED"),
                Some("Command \"npm run build\" exited with 1")
            )
        );
        assert_eq!(
            (err.inspector_url.as_deref(), err.url.as_deref()),
            (
                Some("https://vercel.com/acme/web/err"),
                Some("https://web-err.vercel.app")
            )
        );
        let created = err.created_ms.unwrap();
        assert_eq!(
            (err.building_ms, err.ready_ms),
            (Some(created + 10_000), Some(created + 100_000))
        );

        assert!(
            ok.current && ok.rollback_ok && !prev.current && prev.rollback_ok && !old.rollback_ok
        );
        assert_eq!(preview.env.as_str(), "preview");
        assert_eq!(
            (
                build.state.as_str(),
                build.url.as_ref(),
                build.building_ms.is_some()
            ),
            ("building", None, true)
        );

        let pages = list(&state, Some(site.id), None).unwrap();
        assert_eq!(
            pages
                .iter()
                .map(|dep| (dep.id.as_str(), dep.state.as_str(), dep.env.as_str()))
                .collect::<Vec<_>>(),
            [
                ("pg_ok", "ready", "production"),
                ("pg_fail", "error", "preview"),
                ("pg_skipped", "canceled", "preview")
            ]
        );
        let pg_ok = pages.iter().find(|dep| dep.id == "pg_ok").unwrap();
        assert!(pg_ok.current, "最新那次成功的生产部署就是当前版本");
        assert_eq!(
            (pg_ok.created_ms, pg_ok.ready_ms),
            (Some(1_795_428_000_123), Some(1_795_428_120_000))
        );
        assert_eq!(
            (pg_ok.url.as_deref(), pg_ok.branch.as_deref()),
            (Some("https://site-ok.pages.dev"), Some("main"))
        );
        assert!(!pages[1].current);

        let builds = list(&state, Some(worker.id), None).unwrap();
        assert_eq!(
            builds
                .iter()
                .map(|dep| (dep.id.as_str(), dep.state.as_str(), dep.env.as_str()))
                .collect::<Vec<_>>(),
            [
                ("wk_run", "building", "production"),
                ("wk_ok", "ready", "production"),
                ("wk_fail", "error", "preview")
            ]
        );
        let wk_fail = builds.iter().find(|dep| dep.id == "wk_fail").unwrap();
        assert_eq!(
            (
                wk_fail.error_code.as_deref(),
                wk_fail.error_message.as_deref(),
                wk_fail.branch.as_deref()
            ),
            (
                Some("failure"),
                Some("Build command failed"),
                Some("feature/z")
            )
        );
        let wk_ok = builds.iter().find(|dep| dep.id == "wk_ok").unwrap();
        assert_eq!(
            wk_ok.url.as_deref(),
            Some(fake.site("").as_str()),
            "生产构建的地址就是 Worker 自己的地址"
        );

        // 活动流：到了终态的各记一条
        let events = events_of(&state);
        let has = |kind: &str, level: &str, needle: &str| {
            events
                .iter()
                .any(|(k, l, message)| k == kind && l == level && message.contains(needle))
        };
        assert!(
            has(
                "deploy.failed",
                "error",
                "生产部署失败：1111111 break the build（BUILD_FAILED：Command \"npm run build\" exited with 1）"
            ),
            "{events:?}"
        );
        assert!(has("deploy.ready", "info", "生产部署就绪：abcdef1 ship it"));
        assert!(has(
            "deploy.ready",
            "info",
            "生产部署就绪：aaaaaa1 ship pages"
        ));
        assert!(
            has(
                "deploy.failed",
                "error",
                "预览部署失败：dddddd4 wip（failure：Build command failed）"
            ),
            "{events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|(kind, _, message)| kind == "deploy.failed" && message.contains("docs only")),
            "被跳过的部署不算失败"
        );

        // 刷新失败（比如令牌失效）：记一次警告，同一个原因不重复记
        state
            .secrets
            .set("deploy_cloudflare_token", "expired")
            .unwrap();
        assert!(
            refresh(&state, Some(site.id))
                .await
                .unwrap_err()
                .contains("令牌无效")
        );
        assert!(
            refresh(&state, Some(site.id))
                .await
                .unwrap_err()
                .contains("令牌无效")
        );
        let warnings = events_of(&state)
            .into_iter()
            .filter(|(kind, level, _)| kind == "deploy.refresh" && level == "warning")
            .count();
        assert_eq!(warnings, 1);
        state
            .secrets
            .set("deploy_cloudflare_token", "cf_good")
            .unwrap();
        assert_eq!(refresh(&state, Some(site.id)).await.unwrap(), 0);
        assert_eq!(
            events_of(&state)
                .into_iter()
                .filter(|(kind, _, _)| kind == "deploy.refresh")
                .count(),
            1,
            "原因变了才会再记"
        );

        // 平台上的状态变了再拉：只算有变化的
        set(&fake.vercel, "dpl_build", "readyState", "CANCELED");
        assert_eq!(refresh(&state, Some(web.id)).await.unwrap(), 1);
        let build = list(&state, Some(web.id), None)
            .unwrap()
            .into_iter()
            .find(|dep| dep.id == "dpl_build")
            .unwrap();
        assert_eq!(build.state, "canceled");
        assert_eq!(list(&state, None, Some(4)).unwrap().len(), 4);
    }

    #[tokio::test]
    async fn a_failed_production_deploy_becomes_a_repair_task_that_the_agent_works_on() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = setup_repo_at(tmp.path(), "octo", "demo");
        let (bin, service) = fake_agent(AGENT);
        let (fake, state) = rigged(service).await;
        state.secrets.set("github_pat", "ghp_test").unwrap();
        seed(&fake);
        // 这个仓库之前处理过：记得在哪台机器、用哪个 Agent、仓库在哪
        let earlier = {
            let db = state.db.lock().unwrap();
            let earlier =
                tasks::create_manual(&db, "早先的任务", "octo/demo", "随便做点事").unwrap();
            db.execute(
                "UPDATE tasks SET node_id = 'local', adapter = 'claude-cli', workdir = ?2 WHERE id = ?1",
                params![earlier.id, repo.to_str().unwrap()],
            )
            .unwrap();
            earlier.id
        };

        let web = add_target(
            &state,
            "vercel",
            "web",
            "prj_web",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        let deps = list(&state, Some(web.id), None).unwrap();
        let failed = deps.iter().find(|dep| dep.id == "dpl_err").unwrap();
        let ok = deps.iter().find(|dep| dep.id == "dpl_ok").unwrap();
        let preview = deps.iter().find(|dep| dep.id == "dpl_preview").unwrap();
        assert!(failed.guarded && ok.guarded && preview.guarded);
        assert_eq!(ok.health.as_deref(), Some("ok"));
        assert_eq!(preview.health, None, "预览部署不做健康检查");
        assert!(
            preview.task_id.is_none() && ok.task_id.is_none(),
            "只有出问题的那次才带修复任务"
        );

        // 修复任务：标题点明是哪个项目哪次提交，说明里有原因和构建日志
        let task_id = failed.task_id.expect("失败的生产部署要带上修复任务");
        let task = tasks::load(&state.db.lock().unwrap(), task_id).unwrap();
        assert_eq!(
            (task.repo.as_deref(), task.source.as_str()),
            (Some("octo/demo"), "manual")
        );
        assert_eq!(task.title, "修复 web 部署失败（1111111）");
        let instructions = task.instructions.clone().unwrap();
        for needle in [
            "Vercel 上的项目「web」的生产部署出了问题",
            "分支：main",
            "提交：1111111 break the build",
            "BUILD_FAILED：Command \"npm run build\" exited with 1",
            "Type error: Cannot find name 'useEffect'",
        ] {
            assert!(
                instructions.contains(needle),
                "说明里缺「{needle}」：\n{instructions}"
            );
        }
        assert!(
            instructions.contains("构建日志的最后部分"),
            "{instructions}"
        );

        // 沿用上次的机器与 Agent 自动开工：基于出问题的分支，改完停下等人确认
        let task = wait_status(&state, task_id, &["changes", "failed"]).await;
        assert_eq!(
            task.status,
            "changes",
            "{}",
            task.failure.unwrap_or_default()
        );
        let canonical = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(
            (
                task.node_id.as_deref(),
                task.adapter.as_deref(),
                task.workdir.as_deref()
            ),
            (
                Some("local"),
                Some("claude-cli"),
                Some(canonical.to_str().unwrap())
            )
        );
        assert_eq!(task.base_branch.as_deref(), Some("main"));
        assert!(!task.auto_pr, "修复也要先让人看过改动");
        let worktree = task.worktree.clone().unwrap();
        // 工作树在仓库旁边，以仓库目录的名字命名
        assert!(
            worktree.ends_with(&format!("app-wt/agent-task-{task_id}")),
            "{worktree}"
        );
        assert!(
            std::path::Path::new(&worktree).join("fix.txt").exists(),
            "Agent 没有在工作树里改东西：{worktree}"
        );
        assert!(task.session_id.is_some(), "Agent 的往来要留在会话里");

        let events = events_of(&state);
        assert!(
            events
                .iter()
                .any(|(kind, level, message)| kind == "deploy.repair"
                    && level == "warning"
                    && message.contains(&format!("已建修复任务 #{task_id}"))),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|(kind, _, message)| kind == "task.started"
                    && message.contains("Agent 开始处理")),
            "{events:?}"
        );

        // 再怎么刷新也不会为同一次部署重复建任务
        assert_eq!(refresh(&state, Some(web.id)).await.unwrap(), 0);
        let pending: Vec<i64> = tasks::in_status(&state.db.lock().unwrap(), "pending")
            .unwrap()
            .into_iter()
            .map(|task| task.id)
            .collect();
        assert_eq!(pending, [earlier]);
        assert_eq!(
            list(&state, None, None)
                .unwrap()
                .iter()
                .filter(|dep| dep.task_id.is_some())
                .count(),
            1
        );

        // 没处理过这个仓库：任务建好，等人挑机器和 Agent
        let plain_repo =
            tasks::create_manual(&state.db.lock().unwrap(), "first", "someone/other", "x").unwrap();
        assert_eq!(plain_repo.status, "pending");
        let _ = bin;
    }

    /// PR 合并进生产分支：开「合并即部署」的目标自动发起部署；别的仓库/别的分支不触发；
    /// 平台已经在部署这个提交就不重复；pipeline 看到 PR 合并也会走到这里
    #[tokio::test]
    async fn merged_pr_triggers_auto_deploy_and_dedupes() {
        let (fake, state) = rigged(idle()).await;
        seed(&fake);
        *fake.pulls.lock().unwrap() = json!({
            "number": 9, "state": "closed", "merged": true, "merged_at": "2026-11-26T00:00:00Z",
            "head": {"sha": "head123"}, "base": {"ref": "main"}, "merge_commit_sha": "mrg123",
            "html_url": "https://github.test/octo/demo/pull/9"
        });
        let web = add_target(&state, "vercel", "web", "prj_web", None, None)
            .await
            .unwrap();
        // 开「合并即部署」（参数顺位：url / 生产确认 / 自动修复 / 自动 PR / 合并即部署 / 自动回滚 …）
        update_target(
            &state,
            web.id,
            None,
            None,
            None,
            None,
            Some(true),
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // 别的仓库、别的分支：不动
        on_pr_merged(&state, "octo/other", "main", Some("mrg123")).await;
        on_pr_merged(&state, "octo/demo", "feature", Some("mrg123")).await;
        assert!(fake.call("POST", "/v13/deployments").is_none());

        // 合并进 main：自动发起部署，提交指定的是合并提交
        on_pr_merged(&state, "octo/demo", "main", Some("mrg123")).await;
        let body = fake.body("POST", "/v13/deployments");
        assert_eq!(body["gitSource"]["sha"], "mrg123", "{body}");
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, _, message)| kind == "deploy.auto"
                    && message.contains("自动发起了部署"))
        );

        // 平台已经在部署这个提交了：不再重复触发
        let before = fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == "POST" && p.contains("/v13/deployments"))
            .count();
        fake.vercel.lock().unwrap().insert(
            0,
            json!({"uid": "dpl_merged", "projectId": "prj_web", "name": "web", "readyState": "READY",
                   "target": "production", "created": now_ms(), "ready": now_ms(),
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "mrg123", "githubCommitMessage": "merge pr 9"}}),
        );
        refresh(&state, Some(web.id)).await.unwrap();
        on_pr_merged(&state, "octo/demo", "main", Some("mrg123")).await;
        let after = fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == "POST" && p.contains("/v13/deployments"))
            .count();
        assert_eq!(before, after, "同一个提交不该重复触发");
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, _, message)| kind == "deploy.auto" && message.contains("不重复触发"))
        );

        // 集成：任务流水线看到 PR 合并 → 自动部署（决策中心点合并走的是同一条路）。
        // 换一个新的合并提交：mrg123 平台已经在部署，会被去重拦住
        fake.pulls.lock().unwrap()["merge_commit_sha"] = json!("mrg999");
        state.secrets.set("github_pat", "gh-token").unwrap();
        let task = tasks::create_manual(
            &state.db.lock().unwrap(),
            "上线搜索",
            "octo/demo",
            "把搜索做了",
        )
        .unwrap();
        {
            let db = state.db.lock().unwrap();
            db.execute(
                "UPDATE tasks SET pr_number = 9, status = 'reviewing' WHERE id = ?1",
                params![task.id],
            )
            .unwrap();
        }
        pipeline::refresh(&state, task.id).await.unwrap();
        let task = tasks::load(&state.db.lock().unwrap(), task.id).unwrap();
        assert_eq!(task.status, "done");
        let triggered = fake
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == "POST" && p.contains("/v13/deployments"))
            .count();
        assert_eq!(triggered, after + 1, "合并之后又发起了一次部署");
    }

    /// 部署成功但健康没过、目标开着自动回滚：先回到已知健康的部署止血，修复任务照建
    #[tokio::test]
    async fn unhealthy_production_rolls_back_and_still_repairs() {
        let (fake, state) = rigged(idle()).await;
        seed(&fake);
        // 先用健康页登记：dpl_ok 过了检查，成了「已知健康」的候选
        let web = add_target(
            &state,
            "vercel",
            "web",
            "prj_web",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        assert!(
            list(&state, Some(web.id), None)
                .unwrap()
                .iter()
                .any(|dep| dep.id == "dpl_ok" && dep.health.as_deref() == Some("ok"))
        );

        // 生产地址挂了 + 开自动回滚，推一条新部署上来
        update_target(
            &state,
            web.id,
            Some(&fake.site("/down")),
            None,
            None,
            None,
            None,
            Some(true),
            None,
            None,
            None,
        )
        .unwrap();
        fake.vercel.lock().unwrap().insert(
            0,
            json!({"uid": "dpl_new_bad", "projectId": "prj_web", "name": "web", "readyState": "READY",
                   "readySubstate": "PROMOTED", "target": "production", "created": now_ms(), "ready": now_ms(),
                   "isRollbackCandidate": true,
                   "meta": {"githubCommitRef": "main", "githubCommitSha": "9999999eee", "githubCommitMessage": "bad ship"}}),
        );
        refresh(&state, Some(web.id)).await.unwrap();

        // 回滚到已知健康的 dpl_ok（不是 dpl_prev：它没被我们见过健康）
        let (path, _) = fake
            .call("POST", "/rollback/dpl_ok")
            .expect("应该回滚到 dpl_ok");
        assert!(path.contains("teamId"), "{path}");
        let bad = load_deployment(&state.db.lock().unwrap(), "dpl_new_bad").unwrap();
        assert!(
            bad.health.as_deref().unwrap().contains("HTTP 500"),
            "{:?}",
            bad.health
        );
        assert!(bad.task_id.is_some(), "回滚了也照样建修复任务");
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, level, message)| kind == "deploy.auto"
                    && level == "warning"
                    && message.contains("自动回滚止血"))
        );
    }

    #[tokio::test]
    async fn health_checks_and_limits_decide_whether_to_start_a_repair() {
        let (fake, state) = rigged(idle()).await;
        seed(&fake);
        // web 的生产地址挂了：部署成功，健康检查不过
        let web = add_target(
            &state,
            "vercel",
            "web",
            "prj_web",
            Some(&fake.site("/down")),
            None,
        )
        .await
        .unwrap();
        let ok = list(&state, Some(web.id), None)
            .unwrap()
            .into_iter()
            .find(|dep| dep.id == "dpl_ok")
            .unwrap();
        assert_eq!(ok.health.as_deref(), Some("HTTP 500"));
        let task_id = ok.task_id.expect("健康检查不过也要建修复任务");
        let task = tasks::load(&state.db.lock().unwrap(), task_id).unwrap();
        let instructions = task.instructions.unwrap();
        assert!(
            instructions.contains("部署成功了，但健康检查没通过：HTTP 500"),
            "{instructions}"
        );
        assert!(
            !instructions.contains("构建日志"),
            "构建是成功的，没有日志可带：{instructions}"
        );
        assert!(
            instructions.contains("查的是 "),
            "要说清楚查的是哪个地址：{instructions}"
        );
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, level, message)| kind == "deploy.repair"
                    && level == "warning"
                    && message.contains("健康检查没通过：HTTP 500"))
        );
        // 任务留着等人工启动（这个仓库还没处理过）
        assert_eq!(task.status, "pending");

        // 有访问保护的地址：检查不了，不算出事
        let alt = add_target(
            &state,
            "vercel",
            "alt",
            "prj_alt",
            Some(&fake.site("/private")),
            None,
        )
        .await
        .unwrap();
        let alt_ok = list(&state, Some(alt.id), None).unwrap().remove(0);
        assert!(
            alt_ok.health.as_deref().unwrap().contains("访问保护"),
            "{:?}",
            alt_ok.health
        );
        assert!(alt_ok.guarded && alt_ok.task_id.is_none());
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, level, message)| kind == "deploy.health"
                    && level == "warning"
                    && message.contains("健康检查没法做"))
        );

        // 连不上（多半是这台机器的网络问题）：同样不算
        let unreachable = add_target(
            &state,
            "vercel",
            "u",
            "prj_u",
            Some("http://127.0.0.1:9/healthz"),
            None,
        )
        .await
        .unwrap();
        let unreachable_ok = list(&state, Some(unreachable.id), None).unwrap().remove(0);
        assert!(
            unreachable_ok.health.as_deref().unwrap().contains("连不上"),
            "{:?}",
            unreachable_ok.health
        );
        assert!(unreachable_ok.task_id.is_none(), "连不上不该建修复任务");

        let off = add_target(
            &state,
            "vercel",
            "off",
            "prj_off",
            Some(&fake.site("/down")),
            None,
        )
        .await
        .unwrap();
        let first = list(&state, Some(off.id), None).unwrap().remove(0);
        assert!(first.task_id.is_some(), "开着自动修复时会建任务");
        let push = |suffix: &str| {
            let mut dep = json!({"uid": format!("dpl_off_{suffix}"), "projectId": "prj_off", "name": "off", "readyState": "ERROR", "target": "production",
                                 "created": now_ms(), "errorCode": "BUILD_FAILED", "errorMessage": "exit 1",
                                 "meta": {"githubCommitRef": "main", "githubCommitSha": "5555555fff"}});
            dep["created"] = json!(now_ms());
            fake.vercel.lock().unwrap().insert(0, dep);
        };

        // 没开自动修复：只提醒，不建任务
        update_target(
            &state,
            off.id,
            None,
            None,
            Some(false),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        push("second");
        refresh(&state, Some(off.id)).await.unwrap();
        let second = load_deployment(&state.db.lock().unwrap(), "dpl_off_second").unwrap();
        assert!(second.guarded && second.task_id.is_none());
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, level, message)| kind == "deploy.guard"
                    && level == "warning"
                    && message.contains("没有开自动修复"))
        );

        // 24 小时内已经建过三个：不再建
        {
            let db = state.db.lock().unwrap();
            for i in 0..2 {
                db.execute(
                    "INSERT INTO deployments (id, target_id, provider, project, env, state, raw_state, created_ms, guarded, task_id) \
                     VALUES (?1, ?2, 'vercel', 'off', 'production', 'error', 'ERROR', ?3, 1, 77)",
                    params![format!("dpl_off_seen{i}"), off.id, now_ms() - 60_000],
                )
                .unwrap();
            }
        }
        update_target(
            &state,
            off.id,
            None,
            None,
            Some(true),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        push("third");
        refresh(&state, Some(off.id)).await.unwrap();
        let third = load_deployment(&state.db.lock().unwrap(), "dpl_off_third").unwrap();
        assert!(third.guarded && third.task_id.is_none());
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, level, message)| kind == "deploy.guard"
                    && level == "warning"
                    && message.contains("不再自动建"))
        );

        // 没关联 GitHub 仓库：建不了任务
        let plain = add_target(
            &state,
            "vercel",
            "plain",
            "prj_plain",
            Some(&fake.site("/down")),
            None,
        )
        .await
        .unwrap();
        assert!(
            list(&state, Some(plain.id), None).unwrap().is_empty(),
            "plain 在假平台上还没有部署"
        );
        let mut plain_dep = json!({"uid": "dpl_plain1", "projectId": "prj_plain", "readyState": "READY", "target": "production", "created": now_ms(), "meta": {}});
        plain_dep["created"] = json!(now_ms());
        fake.vercel.lock().unwrap().push(plain_dep);
        refresh(&state, Some(plain.id)).await.unwrap();
        let plain_ok = list(&state, Some(plain.id), None).unwrap().remove(0);
        assert_eq!(plain_ok.health.as_deref(), Some("HTTP 500"));
        assert!(plain_ok.task_id.is_none());
        assert!(
            events_of(&state)
                .iter()
                .any(|(kind, _, message)| kind == "deploy.guard"
                    && message.contains("没有关联 GitHub 仓库"))
        );

        // 手动复查一次健康
        update_target(
            &state,
            web.id,
            Some(&fake.site("")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(check_health(&state, "dpl_ok").await.unwrap(), "ok");
        assert!(check_health(&state, "dpl_nope").await.is_err());
    }

    #[tokio::test]
    async fn production_deploys_need_a_confirm_and_requests_match_the_documented_shapes() {
        let (fake, state) = rigged(idle()).await;
        seed(&fake);
        let web = add_target(
            &state,
            "vercel",
            "web",
            "prj_web",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();

        // 生产分支：要先确认
        let err = trigger(&state, web.id, Some("main"), None, false)
            .await
            .unwrap_err();
        assert!(err.contains("需要先确认"), "{err}");
        assert!(
            fake.call("POST", "/v13/deployments").is_none(),
            "没确认就不该发请求"
        );
        let gate_off = update_target(
            &state,
            web.id,
            None,
            Some(false),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(!gate_off.prod_requires_approval);
        let dep = trigger(&state, web.id, Some(" main "), None, false)
            .await
            .unwrap();
        assert!(dep.id.starts_with("dpl_new"), "{}", dep.id);
        let (path, body) = fake.call("POST", "/v13/deployments").unwrap();
        assert_eq!(path, "/v13/deployments?teamId=team_a");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            body,
            json!({"name": "web", "project": "prj_web", "target": "production", "gitSource": {"type": "github", "ref": "main", "repoId": "1234"}})
        );
        assert!(
            (dep.env.as_str(), dep.state.as_str(), dep.branch.as_deref())
                == ("production", "queued", Some("main"))
        );
        update_target(
            &state,
            web.id,
            None,
            Some(true),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

        // 预览分支 + 指定提交：不用确认，不带 target
        let dep = trigger(
            &state,
            web.id,
            Some("feature/x"),
            Some("fffff0000full"),
            false,
        )
        .await
        .unwrap();
        assert_eq!(dep.env, "preview");
        let body = fake.body("POST", "/v13/deployments");
        assert_eq!(
            body["gitSource"],
            json!({"type": "github", "ref": "feature/x", "sha": "fffff0000full", "repoId": "1234"})
        );
        assert!(body.get("target").is_none());

        // 没有仓库 id 的项目用 org + repo
        let mut alt = add_target(
            &state,
            "vercel",
            "alt",
            "prj_alt",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        alt.repo_id = None;
        save_target(&state.db.lock().unwrap(), &alt).unwrap();
        trigger(&state, alt.id, None, None, false)
            .await
            .unwrap_err();
        trigger(&state, alt.id, Some("dev"), None, false)
            .await
            .unwrap();
        assert_eq!(
            fake.body("POST", "/v13/deployments")["gitSource"],
            json!({"type": "github", "ref": "dev", "org": "octo", "repo": "demo"})
        );

        // 没关联 GitHub 的项目：只能把最近一次原样再来一遍
        let plain = add_target(
            &state,
            "vercel",
            "plain",
            "prj_plain",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        let err = trigger(&state, plain.id, Some("main"), None, true)
            .await
            .unwrap_err();
        assert!(err.contains("还没有部署过"), "{err}");
        let mut plain_dep = json!({"uid": "dpl_plain1", "projectId": "prj_plain", "readyState": "READY", "target": "production", "created": now_ms(), "meta": {"githubCommitRef": "main"}});
        plain_dep["created"] = json!(now_ms());
        fake.vercel.lock().unwrap().push(plain_dep);
        refresh(&state, Some(plain.id)).await.unwrap();
        assert!(
            trigger(&state, plain.id, Some("dev"), None, true)
                .await
                .unwrap_err()
                .contains("没有关联 GitHub 仓库")
        );
        trigger(&state, plain.id, Some("main"), None, true)
            .await
            .unwrap();
        assert_eq!(
            fake.body("POST", "/v13/deployments"),
            json!({"name": "plain", "project": "prj_plain", "deploymentId": "dpl_plain1", "withLatestCommit": true})
        );

        // 重新部署一次生产部署也要确认
        assert!(
            retry(&state, "dpl_ok", false)
                .await
                .unwrap_err()
                .contains("需要先确认")
        );
        let again = retry(&state, "dpl_ok", true).await.unwrap();
        assert!(again.id.starts_with("dpl_new"));
        assert_eq!(
            fake.body("POST", "/v13/deployments"),
            json!({"name": "web", "project": "prj_web", "deploymentId": "dpl_ok", "withLatestCommit": true})
        );

        // 取消
        let building = list(&state, Some(web.id), None)
            .unwrap()
            .into_iter()
            .find(|dep| dep.state == "building")
            .unwrap();
        cancel(&state, &building.id).await.unwrap();
        assert_eq!(
            fake.call("PATCH", "/cancel").unwrap().0,
            format!("/v12/deployments/{}/cancel?teamId=team_a", building.id)
        );
        assert_eq!(
            load_deployment(&state.db.lock().unwrap(), &building.id)
                .unwrap()
                .state,
            "canceled"
        );

        // 把预览部署提升为生产
        assert!(
            promote(&state, "dpl_build")
                .await
                .unwrap_err()
                .contains("只有成功的部署")
        );
        promote(&state, "dpl_preview").await.unwrap();
        assert_eq!(
            fake.call("POST", "/promote/").unwrap().0,
            "/v10/projects/prj_web/promote/dpl_preview?teamId=team_a"
        );

        // 回滚候选：平台说能回滚的成功生产部署
        let candidates = history(&state, web.id).await.unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|item| (item.id.as_str(), item.current))
                .collect::<Vec<_>>(),
            [("dpl_ok", true), ("dpl_prev", false)]
        );
        assert!(candidates.iter().all(|item| item.version_id.is_none()));
        // 平台没给标记的时候，成功的生产部署都算
        {
            let db = state.db.lock().unwrap();
            db.execute(
                "UPDATE deployments SET rollback_ok = 0 WHERE target_id = ?1",
                params![web.id],
            )
            .unwrap();
        }
        assert_eq!(history(&state, web.id).await.unwrap().len(), 3);
        rollback(&state, web.id, "dpl_prev", None).await.unwrap();
        assert_eq!(
            fake.call("POST", "/rollback/").unwrap().0,
            "/v1/projects/prj_web/rollback/dpl_prev?description=AutoAgents+rollback&teamId=team_a"
        );
        let current: Vec<String> = list(&state, Some(web.id), None)
            .unwrap()
            .into_iter()
            .filter(|dep| dep.current)
            .map(|dep| dep.id)
            .collect();
        assert_eq!(current, ["dpl_prev"]);

        // 平台报错时把原因带回来
        assert!(promote(&state, "dpl_nope").await.is_err());
        assert!(rollback(&state, web.id, "dpl_nope", None).await.is_err());
        assert!(load_deployment(&state.db.lock().unwrap(), "dpl_nope").is_err());

        let events = events_of(&state);
        for needle in [
            "发起了生产部署：main",
            "发起了预览部署：feature/x",
            "取消了预览部署",
            "提升为生产",
            "生产部署已回滚",
        ] {
            assert!(
                events
                    .iter()
                    .any(|(_, _, message)| message.contains(needle)),
                "缺事件：{needle}"
            );
        }
    }

    #[tokio::test]
    async fn pages_and_workers_have_their_own_ways_to_deploy_and_roll_back() {
        let (fake, state) = rigged(idle()).await;
        seed(&fake);
        let site = add_target(
            &state,
            "cloudflare-pages",
            "site",
            "site",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();

        // Pages：没有指定分支发起部署的接口，只能把这条分支最近的一次原样再来一遍
        let err = trigger(&state, site.id, Some("feature/new"), None, false)
            .await
            .unwrap_err();
        assert!(err.contains("还没有部署过"), "{err}");
        let preview = trigger(&state, site.id, Some("feature/y"), None, false)
            .await
            .unwrap();
        assert!(preview.id.starts_with("pg_new"), "{}", preview.id);
        assert_eq!(preview.branch.as_deref(), Some("feature/y"));
        assert!(
            fake.call("POST", "/retry")
                .unwrap()
                .0
                .ends_with("/pages/projects/site/deployments/pg_fail/retry")
        );
        assert!(
            trigger(&state, site.id, Some("main"), None, false)
                .await
                .unwrap_err()
                .contains("需要先确认")
        );
        let production = trigger(&state, site.id, Some("main"), None, true)
            .await
            .unwrap();
        assert_eq!(production.env, "production");
        assert!(
            fake.call("POST", "/retry")
                .unwrap()
                .0
                .ends_with("/deployments/pg_ok/retry")
        );

        // Pages 没有取消接口；回滚是把生产切回某一次
        assert!(
            cancel(&state, "pg_ok")
                .await
                .unwrap_err()
                .contains("没有取消部署的接口")
        );
        assert_eq!(
            history(&state, site.id)
                .await
                .unwrap()
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["pg_ok"]
        );
        rollback(&state, site.id, "pg_ok", None).await.unwrap();
        assert_eq!(
            fake.call("POST", "/rollback").unwrap().0,
            "/accounts/acct1/pages/projects/site/deployments/pg_ok/rollback"
        );
        let failed = load_deployment(&state.db.lock().unwrap(), "pg_fail").unwrap();
        let pages_log = logs(&state, &failed).await.unwrap();
        assert!(
            pages_log.contains("✘ [ERROR] Build failed with 1 error"),
            "{pages_log}"
        );

        // Workers：没有 Deploy Hook 就没法主动触发
        let worker = add_target(
            &state,
            "cloudflare-workers",
            "api-worker",
            "tag_api_worker",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        assert!(
            trigger(&state, worker.id, None, None, true)
                .await
                .unwrap_err()
                .contains("Deploy Hook")
        );
        // 真实的 Deploy Hook 是 https 的（产品要求）；假平台的地址只能直接放进凭据库
        assert!(
            update_target(
                &state,
                worker.id,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                Some("http://insecure/hook")
            )
            .is_err()
        );
        let hook = format!("{}/workers/builds/deploy_hooks/hook123", fake.addr);
        state.secrets.set(&hook_account(worker.id), &hook).unwrap();
        update_target(
            &state,
            worker.id,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("https://hooks.cloudflare.example/x"),
        )
        .unwrap();
        state.secrets.set(&hook_account(worker.id), &hook).unwrap();
        assert!(
            trigger(&state, worker.id, None, None, false)
                .await
                .unwrap_err()
                .contains("需要先确认"),
            "Hook 多半构建的是生产分支"
        );
        let build = trigger(&state, worker.id, None, None, true).await.unwrap();
        assert_eq!(build.id, "wk_new");
        assert_eq!(build.state, "queued");
        assert_eq!(
            fake.call("POST", "/deploy_hooks/").unwrap().0,
            "/workers/builds/deploy_hooks/hook123"
        );

        let failed = load_deployment(&state.db.lock().unwrap(), "wk_fail").unwrap();
        let build_log = logs(&state, &failed).await.unwrap();
        assert_eq!(build_log, "Running build command\n✘ [ERROR] build failed");
        cancel(&state, "wk_run").await.unwrap();
        assert_eq!(
            fake.call("PUT", "/cancel").unwrap().0,
            "/accounts/acct1/builds/builds/wk_run/cancel"
        );
        // 重新部署也是走 Hook
        assert!(
            retry(&state, "wk_ok", false)
                .await
                .unwrap_err()
                .contains("需要先确认")
        );
        retry(&state, "wk_ok", true).await.unwrap();
        assert_eq!(
            fake.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(method, path, _)| method == "POST" && path.contains("/deploy_hooks/"))
                .count(),
            2
        );

        // 回滚的单位是版本：列出历次部署，按时间新的在前
        let candidates = history(&state, worker.id).await.unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|item| (
                    item.id.as_str(),
                    item.version_id.as_deref(),
                    item.current,
                    item.commit_message.as_deref()
                ))
                .collect::<Vec<_>>(),
            [
                ("wdep2", Some("ver2"), true, Some("bump deps")),
                ("wdep1", Some("ver1"), false, Some("add health endpoint"))
            ]
        );
        assert!(
            rollback(&state, worker.id, "wdep1", None)
                .await
                .unwrap_err()
                .contains("指定")
        );
        rollback(&state, worker.id, "wdep1", Some("ver1"))
            .await
            .unwrap();
        assert_eq!(
            fake.body("POST", "/scripts/api-worker/deployments"),
            json!({"strategy": "percentage", "versions": [{"percentage": 100, "version_id": "ver1"}], "annotations": {"workers/message": "AutoAgents 回滚"}})
        );
        assert!(
            promote(&state, "wk_ok")
                .await
                .unwrap_err()
                .contains("只有 Vercel")
        );
    }

    #[tokio::test]
    async fn deployments_link_back_to_the_tasks_that_produced_them() {
        let (fake, state) = rigged(idle()).await;
        seed(&fake);
        let web = add_target(
            &state,
            "vercel",
            "web",
            "prj_web",
            Some(&fake.site("")),
            None,
        )
        .await
        .unwrap();
        let by_branch =
            tasks::create_manual(&state.db.lock().unwrap(), "t1", "octo/demo", "x").unwrap();
        let by_commit =
            tasks::create_manual(&state.db.lock().unwrap(), "t2", "octo/demo", "x").unwrap();
        let elsewhere =
            tasks::create_manual(&state.db.lock().unwrap(), "t3", "someone/else", "x").unwrap();
        {
            let db = state.db.lock().unwrap();
            db.execute(
                "UPDATE tasks SET branch = 'feature/x', head_sha = 'fffff0000' WHERE id = ?1",
                params![by_branch.id],
            )
            .unwrap();
            db.execute("UPDATE tasks SET branch = 'no-such-branch', head_sha = 'abcdef123456' WHERE id = ?1", params![by_commit.id]).unwrap();
        }
        // 按分支对上，或按推上去的提交对上
        let ids = |task: i64| {
            for_task(&state, task)
                .unwrap()
                .into_iter()
                .map(|dep| dep.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(by_branch.id), ["dpl_build", "dpl_preview"]);
        assert_eq!(ids(by_commit.id), ["dpl_ok"]);
        assert!(ids(elsewhere.id).is_empty());
        // 守护建的修复任务还没有分支：靠 deployments.task_id 找到触发它的那次部署
        let repair =
            tasks::create_manual(&state.db.lock().unwrap(), "t4", "octo/demo", "x").unwrap();
        assert!(ids(repair.id).is_empty());
        state
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE deployments SET task_id = ?1 WHERE id = 'dpl_err'",
                params![repair.id],
            )
            .unwrap();
        assert_eq!(ids(repair.id), ["dpl_err"]);
        assert!(for_task(&state, 999).is_err());
        assert_eq!(web.id, 1);
    }

    #[test]
    fn platform_states_and_times_are_normalized() {
        assert_eq!(vercel_state("QUEUED"), ("queued", false));
        assert_eq!(vercel_state("INITIALIZING"), ("queued", false));
        assert_eq!(vercel_state("BUILDING"), ("building", false));
        assert_eq!(vercel_state("READY"), ("ready", true));
        assert_eq!(vercel_state("ERROR"), ("error", true));
        assert_eq!(vercel_state("CANCELED"), ("canceled", true));
        assert_eq!(vercel_state("BLOCKED"), ("other", true));
        assert_eq!(vercel_state("SOMETHING_NEW"), ("other", false));
        assert_eq!(pages_state("idle", false), ("queued", false));
        assert_eq!(pages_state("active", false), ("building", false));
        assert_eq!(pages_state("success", false), ("ready", true));
        assert_eq!(pages_state("failure", false), ("error", true));
        assert_eq!(pages_state("skipped", false), ("canceled", true));
        assert_eq!(pages_state("success", true), ("canceled", true));
        assert_eq!(workers_state("queued", ""), ("queued", false));
        assert_eq!(workers_state("running", ""), ("building", false));
        assert_eq!(workers_state("pending", "pending"), ("queued", false));
        assert_eq!(workers_state("success", "success"), ("ready", true));
        assert_eq!(workers_state("failure", "failure"), ("error", true));
        assert_eq!(workers_state("", "canceled"), ("canceled", true));
        assert_eq!(workers_state("", "skipped"), ("canceled", true));
        assert_eq!(workers_state("", ""), ("other", false));

        assert_eq!(iso_ms("2026-11-24T00:00:00Z"), Some(1_795_478_400_000));
        assert_eq!(iso_ms("2026-11-23T10:00:00.123Z"), Some(1_795_428_000_123));
        assert_eq!(iso_ms("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        assert_eq!(iso_ms("yesterday"), None);

        // 列表接口用 uid，单个部署的接口用 id；时间可能是数字，也可能是写成字符串的数字
        let listed = from_vercel(&json!({"uid": "dpl_1", "readyState": "READY", "readySubstate": "PROMOTED", "target": "production",
                                         "isRollbackCandidate": true, "created": "1609492210000", "url": "web.vercel.app",
                                         "meta": {"githubCommitRef": "main", "githubCommitSha": "abc"}})).unwrap();
        assert!(listed.current && listed.rollback_ok && listed.terminal);
        assert_eq!((listed.env, listed.state), ("production", "ready"));
        assert_eq!(
            (listed.created_ms, listed.url.as_deref()),
            (Some(1_609_492_210_000), Some("https://web.vercel.app"))
        );
        assert_eq!(
            from_vercel(&json!({"id": "dpl_2", "target": null}))
                .unwrap()
                .id,
            "dpl_2"
        );
        assert_eq!(
            from_vercel(&json!({"id": "dpl_2", "target": null}))
                .unwrap()
                .env,
            "preview"
        );
        assert!(
            from_vercel(&json!({"readyState": "READY"})).is_none(),
            "没有编号的条目不要"
        );

        assert_eq!(with_scheme("http://x.test".into()), "http://x.test");
        assert_eq!(
            commit_line(Some("1111111222"), Some(" break the build ")),
            "1111111 break the build"
        );
        assert_eq!(commit_line(None, None), "（没有提交信息）");
        assert_eq!(
            failure_text(Some("BUILD_FAILED"), Some(" exit 1 "), None),
            "BUILD_FAILED：exit 1"
        );
        assert_eq!(failure_text(None, None, Some("ERROR")), "构建失败（ERROR）");
        assert_eq!(clip_tail("一二三四五六七", 3), "…（前面省略）\n五六七");
        assert_eq!(clip_tail("short", 100), "short");
        assert_eq!(iso_ms("2024-01-01T00:00:00Z"), Some(1_704_067_200_000));
        assert_eq!(
            repo_of(Some("octo".into()), Some("demo".into())).as_deref(),
            Some("octo/demo")
        );
        assert_eq!(repo_of(Some("octo".into()), None), None);
        assert_eq!(repo_of(Some(String::new()), Some("demo".into())), None);
    }
}

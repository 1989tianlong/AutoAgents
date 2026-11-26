//! GitHub 接入：PAT 认证（存系统 Keychain）+ REST API。
//!
//! 除了只读查询，任务流水线也走这里：读 Issue 正文、创建 PR、跟踪 PR 与 CI 检查、
//! 取失败的检查日志。

use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::State;

/// PAT 在凭据存储里的名字
const PAT_ACCOUNT: &str = "github_pat";

/// GitHub API 的默认地址；应用状态里可以改掉它（测试指向一个本地的假服务器）
pub const DEFAULT_API: &str = "https://api.github.com";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhUser {
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhRepo {
    pub id: i64,
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
    pub open_issues: i64,
    pub updated_at: Option<String>,
    pub html_url: String,
    /// 合并 PR 的目标分支；没给时任务流水线以它为基础
    pub default_branch: String,
    /// `git@github.com:owner/repo.git`：在机器上克隆私有仓库时用它
    pub ssh_url: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhIssue {
    pub number: i64,
    pub title: String,
    /// 正文（Markdown）；任务流水线把它交给 Agent
    pub body: Option<String>,
    pub state: String,
    pub author: String,
    pub labels: Vec<String>,
    pub updated_at: Option<String>,
    pub html_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhPr {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub draft: bool,
    pub author: String,
    pub head_ref: String,
    pub base_ref: String,
    pub updated_at: Option<String>,
    pub html_url: String,
}

fn pr_from(v: &Value) -> GhPr {
    GhPr {
        number: v["number"].as_i64().unwrap_or_default(),
        title: s(v, "title"),
        state: s(v, "state"),
        draft: v["draft"].as_bool().unwrap_or(false),
        author: s(&v["user"], "login"),
        head_ref: s(&v["head"], "ref"),
        base_ref: s(&v["base"], "ref"),
        updated_at: so(v, "updated_at"),
        html_url: s(v, "html_url"),
    }
}

/// PR 上的一个检查（CI 的一项，或状态接口上报的一个状态）
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GhCheck {
    /// 检查名（如 `build`、`test / linux`）
    pub name: String,
    /// `pending` / `success` / `failure` / `skipped`
    pub state: String,
    /// 详情页（日志）
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PrState {
    pub number: i64,
    /// `open` / `closed`
    pub state: String,
    pub merged: bool,
    pub merged_at: Option<String>,
    /// PR 头部提交
    pub head_sha: String,
    /// 合并进目标分支后的提交（合并后才有了；去重「平台是不是已经在部署它」用）
    pub merge_commit_sha: Option<String>,
    /// PR 的目标分支（合并进的那条）
    pub base_branch: String,
    pub html_url: String,
    pub checks: Vec<GhCheck>,
    pub success: usize,
    pub pending: usize,
    pub failure: usize,
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("AutoAgents/0.1")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

async fn gh_get(state: &AppState, token: &str, path: &str) -> Result<Value, String> {
    gh_call(state, token, "GET", path, None).await
}

/// 发一个请求。失败时把 GitHub 给出的说明带出来（比如「这个分支已经有 PR 了」）
async fn gh_call(
    state: &AppState,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, String> {
    let mut request = http()?
        .request(
            method
                .parse()
                .map_err(|_| format!("HTTP 方法不对：{method}"))?,
            format!("{}{path}", state.gh_api.lock().unwrap()),
        )
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(body) = body {
        request = request.json(&body);
    }
    let resp = request.send().await.map_err(|e| format!("网络错误: {e}"))?;
    let code = resp.status().as_u16();
    if !resp.status().is_success() {
        let value: Option<Value> = resp.json::<Value>().await.ok();
        let detail = value
            .as_ref()
            .and_then(|v| v["message"].as_str())
            .unwrap_or_default();
        let base = match code {
            401 => "Token 无效或已过期（401）".to_string(),
            403 => "访问被拒绝（403），检查 Token 权限或 API 限额".to_string(),
            404 => "资源不存在（404），检查仓库名".to_string(),
            _ => format!("GitHub API 错误（{code}）"),
        };
        return if detail.is_empty() || detail == "Not Found" {
            Err(base)
        } else {
            Err(format!("{base}：{detail}"))
        };
    }
    if code == 204 {
        return Ok(Value::Null);
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("解析响应失败: {e}"))
}

/// 取一段原始文本（不是 JSON 的接口，比如 PR 的 diff：Accept 是 vnd.github.diff）
async fn gh_text(
    state: &AppState,
    token: &str,
    path: &str,
    accept: &str,
) -> Result<String, String> {
    let resp = http()?
        .get(format!("{}{path}", state.gh_api.lock().unwrap()))
        .bearer_auth(token)
        .header("Accept", accept)
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| format!("网络错误: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GitHub API 错误（{}）", resp.status().as_u16()));
    }
    resp.text().await.map_err(|e| format!("读取响应失败: {e}"))
}

/// PR 的完整 diff（unified diff 文本）
pub async fn pr_diff(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<String, String> {
    gh_text(
        state,
        token,
        &format!("/repos/{owner}/{repo}/pulls/{number}"),
        "application/vnd.github.diff",
    )
    .await
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

fn so(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(|x| x.to_string())
}

async fn fetch_user(state: &AppState, token: &str) -> Result<GhUser, String> {
    let v = gh_get(state, token, "/user").await?;
    Ok(GhUser {
        login: s(&v, "login"),
        name: so(&v, "name"),
        avatar_url: so(&v, "avatar_url"),
    })
}

/// 连接 GitHub：先验证 Token，通过后写入 Keychain。
#[tauri::command]
pub async fn github_connect(
    state: State<'_, Arc<AppState>>,
    token: String,
) -> Result<GhUser, String> {
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("请输入 Token".into());
    }
    let user = fetch_user(&state, &token).await?;
    state.secrets.set(PAT_ACCOUNT, &token)?;
    record(
        &state,
        "github.connected",
        "info",
        &user.login,
        "GitHub 已连接",
    );
    Ok(user)
}

/// 查询连接状态：有保存的 Token 就拿它验证一次。
#[tauri::command]
pub async fn github_status(state: State<'_, Arc<AppState>>) -> Result<Option<GhUser>, String> {
    let Some(token) = state.secrets.get(PAT_ACCOUNT)? else {
        return Ok(None);
    };
    Ok(Some(fetch_user(&state, &token).await?))
}

/// 断开连接：删除 Keychain 凭据。
#[tauri::command]
pub async fn github_disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.secrets.delete(PAT_ACCOUNT)?;
    record(
        &state,
        "github.disconnected",
        "info",
        "GitHub",
        "GitHub 已断开连接",
    );
    Ok(())
}

/// 取已保存的 Token；没有就提示先连接
pub fn token_of(state: &AppState) -> Result<String, String> {
    state
        .secrets
        .get(PAT_ACCOUNT)?
        .ok_or_else(|| "尚未连接 GitHub，请先在 GitHub 页连接".to_string())
}

/// 记一条 GitHub 相关的事件（调用方没有持有数据库锁）
pub fn record(state: &AppState, kind: &str, level: &str, source: &str, message: &str) {
    let db = state.db.lock().unwrap();
    crate::events::record(&db, kind, level, source, message, Some("#/github"));
}

/// 我参与的仓库（按更新时间倒序）。
#[tauri::command]
pub async fn github_repos(state: State<'_, Arc<AppState>>) -> Result<Vec<GhRepo>, String> {
    let token = token_of(&state)?;
    let v = gh_get(
        &state,
        &token,
        "/user/repos?sort=updated&per_page=50&affiliation=owner,collaborator,organization_member",
    )
    .await?;
    let mut repos: Vec<GhRepo> = v
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|r| GhRepo {
                    id: r["id"].as_i64().unwrap_or_default(),
                    full_name: s(r, "full_name"),
                    private: r["private"].as_bool().unwrap_or(false),
                    description: so(r, "description"),
                    open_issues: r["open_issues_count"].as_i64().unwrap_or_default(),
                    updated_at: so(r, "updated_at"),
                    html_url: s(r, "html_url"),
                    default_branch: s(r, "default_branch"),
                    ssh_url: s(r, "ssh_url"),
                })
                .collect()
        })
        .unwrap_or_default();
    repos.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(repos)
}

/// Issue 列表（`query` 是 issues 接口的查询串；issues API 混有 PR，过滤掉）。
pub async fn issues_list(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    query: &str,
) -> Result<Vec<GhIssue>, String> {
    let v = gh_get(
        state,
        token,
        &format!("/repos/{owner}/{repo}/issues?{query}"),
    )
    .await?;
    Ok(v.as_array()
        .map(|arr| {
            arr.iter()
                .filter(|i| i.get("pull_request").is_none())
                .map(|i| GhIssue {
                    number: i["number"].as_i64().unwrap_or_default(),
                    title: s(i, "title"),
                    body: so(i, "body").filter(|body| !body.trim().is_empty()),
                    state: s(i, "state"),
                    author: s(&i["user"], "login"),
                    labels: i["labels"]
                        .as_array()
                        .map(|ls| ls.iter().map(|l| s(l, "name")).collect())
                        .unwrap_or_default(),
                    updated_at: so(i, "updated_at"),
                    html_url: s(i, "html_url"),
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Issue 列表（state=all）。
#[tauri::command]
pub async fn github_issues(
    state: State<'_, Arc<AppState>>,
    owner: String,
    repo: String,
) -> Result<Vec<GhIssue>, String> {
    let token = token_of(&state)?;
    issues_list(
        &state,
        &token,
        &owner,
        &repo,
        "state=all&per_page=30&sort=updated",
    )
    .await
}

/// PR 列表（`query` 是 pulls 接口的查询串）。
pub async fn prs_list(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    query: &str,
) -> Result<Vec<GhPr>, String> {
    let v = gh_get(
        state,
        token,
        &format!("/repos/{owner}/{repo}/pulls?{query}"),
    )
    .await?;
    Ok(v.as_array()
        .map(|arr| arr.iter().map(pr_from).collect())
        .unwrap_or_default())
}

/// PR 列表（state=all）。
#[tauri::command]
pub async fn github_prs(
    state: State<'_, Arc<AppState>>,
    owner: String,
    repo: String,
) -> Result<Vec<GhPr>, String> {
    let token = token_of(&state)?;
    prs_list(
        &state,
        &token,
        &owner,
        &repo,
        "state=all&per_page=30&sort=updated&direction=desc",
    )
    .await
}

// ---------------------------------------------------------------------------
// 任务流水线用到的读写接口
// ---------------------------------------------------------------------------

/// 取一份 Issue 的标题与正文
pub async fn issue(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<GhIssue, String> {
    let v = gh_get(
        state,
        token,
        &format!("/repos/{owner}/{repo}/issues/{number}"),
    )
    .await?;
    Ok(GhIssue {
        number,
        title: s(&v, "title"),
        body: so(&v, "body").filter(|body| !body.trim().is_empty()),
        state: s(&v, "state"),
        author: s(&v["user"], "login"),
        labels: Vec::new(),
        updated_at: so(&v, "updated_at"),
        html_url: s(&v, "html_url"),
    })
}

/// 创建 PR。这个分支上已经有 PR 的话直接返回那一个（重试、重复点击都安全）
pub async fn create_pr(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    title: &str,
    head: &str,
    base: &str,
    body: &str,
) -> Result<GhPr, String> {
    let created = gh_call(
        state,
        token,
        "POST",
        &format!("/repos/{owner}/{repo}/pulls"),
        Some(json!({
            "title": title,
            "head": head,
            "base": base,
            "body": body,
        })),
    )
    .await;
    match created {
        Ok(v) => Ok(pr_from(&v)),
        Err(e) if e.contains("already exists") => {
            let v = gh_get(
                state,
                token,
                &format!("/repos/{owner}/{repo}/pulls?head={owner}:{head}&state=open"),
            )
            .await?;
            v.as_array()
                .and_then(|list| list.first())
                .map(pr_from)
                .ok_or_else(|| e.clone())
        }
        Err(e) => Err(e),
    }
}

/// 仓库的默认分支：没指定基础分支时以它为准
pub async fn default_branch(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
) -> Result<String, String> {
    let v = gh_get(state, token, &format!("/repos/{owner}/{repo}")).await?;
    let branch = s(&v, "default_branch");
    if branch.is_empty() {
        Err(format!("GitHub 没有给出 {owner}/{repo} 的默认分支"))
    } else {
        Ok(branch)
    }
}

/// 当前 Token 对应的账号：登录名与数字 id（数字 id 用来拼 noreply 邮箱）
pub async fn whoami(state: &AppState, token: &str) -> Result<(String, i64), String> {
    let v = gh_get(state, token, "/user").await?;
    let login = s(&v, "login");
    if login.is_empty() {
        return Err("GitHub 没有给出当前账号".into());
    }
    Ok((login, v["id"].as_i64().unwrap_or_default()))
}

/// 在已有 PR 的描述后面追加一段（后续修改的说明）
pub async fn append_pr_body(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
    addition: &str,
) -> Result<(), String> {
    let path = format!("/repos/{owner}/{repo}/pulls/{number}");
    let current = gh_get(state, token, &path).await?;
    let mut body = so(&current, "body").unwrap_or_default();
    body.push_str(addition);
    gh_call(state, token, "PATCH", &path, Some(json!({ "body": body })))
        .await
        .map(|_| ())
}

/// 在 PR（Issue 接口）下发一条评论。自动审查的结论贴回 PR 用它
pub async fn comment_pr(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
    body: &str,
) -> Result<(), String> {
    gh_call(
        state,
        token,
        "POST",
        &format!("/repos/{owner}/{repo}/issues/{number}/comments"),
        Some(json!({ "body": body })),
    )
    .await
    .map(|_| ())
}

/// 合并 PR。分支保护、必要的检查没过，GitHub 会拒绝——错误原样带给界面
pub async fn merge_pr(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<(), String> {
    let v = gh_call(
        state,
        token,
        "PUT",
        &format!("/repos/{owner}/{repo}/pulls/{number}/merge"),
        Some(json!({ "merge_method": "squash" })),
    )
    .await?;
    if v.get("merged").and_then(Value::as_bool).unwrap_or(false) {
        Ok(())
    } else {
        Err(format!("GitHub 没有合并这个 PR：{}", v))
    }
}

/// 检查的状态归类
fn check_state(status: &str, conclusion: Option<&str>) -> String {
    match conclusion {
        Some("success") => "success".into(),
        Some("skipped" | "neutral" | "stale") => "skipped".into(),
        Some(_) => "failure".into(),
        None if status == "completed" => "failure".into(),
        None => "pending".into(),
    }
}

/// PR 当前状态与它的检查（CI 的每一项、状态接口上报的状态）
pub async fn pr_state(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<PrState, String> {
    let pr = gh_get(
        state,
        token,
        &format!("/repos/{owner}/{repo}/pulls/{number}"),
    )
    .await?;
    let mut summary = PrState {
        number,
        state: s(&pr, "state"),
        merged: pr["merged"].as_bool().unwrap_or(false),
        merged_at: so(&pr, "merged_at"),
        head_sha: s(&pr["head"], "sha"),
        merge_commit_sha: so(&pr, "merge_commit_sha"),
        base_branch: s(&pr["base"], "ref"),
        html_url: s(&pr, "html_url"),
        ..Default::default()
    };
    if summary.head_sha.is_empty() || summary.merged {
        return Ok(summary);
    }
    let commit = &summary.head_sha;
    // 老式的提交状态（commit status）
    if let Ok(v) = gh_get(
        state,
        token,
        &format!("/repos/{owner}/{repo}/commits/{commit}/status"),
    )
    .await
    {
        for status in v["statuses"].as_array().into_iter().flatten() {
            let raw = s(status, "state");
            let mapped = match raw.as_str() {
                "success" => "success",
                "pending" | "" => "pending",
                _ => "failure",
            };
            summary.checks.push(GhCheck {
                name: s(status, "context"),
                state: mapped.to_string(),
                url: so(status, "target_url"),
            });
        }
    }
    // GitHub Actions 等检查框架上报的检查项（每个 job 一条）
    if let Ok(v) = gh_get(
        state,
        token,
        &format!("/repos/{owner}/{repo}/commits/{commit}/check-runs?per_page=100"),
    )
    .await
    {
        for run in v["check_runs"].as_array().into_iter().flatten() {
            summary.checks.push(GhCheck {
                name: s(run, "name"),
                state: check_state(&s(run, "status"), run["conclusion"].as_str()),
                url: so(run, "html_url"),
            });
        }
    }
    // 同名的检查只留最新的那一条
    let mut seen = std::collections::HashSet::new();
    summary
        .checks
        .retain(|check| seen.insert(check.name.clone()));
    for check in &summary.checks {
        match check.state.as_str() {
            "success" => summary.success += 1,
            "pending" => summary.pending += 1,
            "failure" => summary.failure += 1,
            _ => {}
        }
    }
    Ok(summary)
}

/// 某个 PR 提交上没有通过的检查的日志（每个截取尾部），给 Agent 修 CI 用
pub async fn failed_check_logs(
    state: &AppState,
    token: &str,
    owner: &str,
    repo: &str,
    commit: &str,
) -> Result<Vec<(String, String)>, String> {
    let runs = gh_get(
        state,
        token,
        &format!("/repos/{owner}/{repo}/commits/{commit}/check-runs?per_page=100"),
    )
    .await?;
    let client = http()?;
    let mut logs = Vec::new();
    for run in runs["check_runs"].as_array().into_iter().flatten() {
        let failed = run["conclusion"]
            .as_str()
            .is_some_and(|c| c != "success" && c != "skipped" && c != "neutral");
        if !failed {
            continue;
        }
        let name = s(run, "name");
        // GitHub Actions 的检查项详情页形如 …/actions/runs/<run>/job/<job>
        let job = s(run, "details_url")
            .rsplit("/job/")
            .next()
            .and_then(|id| id.parse::<i64>().ok());
        let Some(job) = job else { continue };
        let resp = client
            .get(format!(
                "{}/repos/{owner}/{repo}/actions/jobs/{job}/logs",
                state.gh_api.lock().unwrap()
            ))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| format!("取检查 {name} 的日志失败：{e}"))?;
        if !resp.status().is_success() {
            continue;
        }
        let text = resp
            .text()
            .await
            .map_err(|e| format!("读取检查 {name} 的日志失败：{e}"))?;
        logs.push((name, clip_tail(&text, LOG_TAIL_CHARS)));
    }
    Ok(logs)
}

/// 日志只留尾部：报错通常在最后
const LOG_TAIL_CHARS: usize = 12_000;

pub fn clip_tail(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let start = text
        .char_indices()
        .rev()
        .nth(max_chars)
        .map_or(0, |(i, _)| i);
    let cut = text[start..].find('\n').map_or(0, |i| i + 1);
    format!("…（前面省略）\n{}", &text[start + cut..])
}

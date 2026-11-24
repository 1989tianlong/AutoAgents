//! GitHub 接入：PAT 认证（存系统 Keychain）+ REST API 查询。

use crate::AppState;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::State;

/// PAT 在凭据存储里的名字
const PAT_ACCOUNT: &str = "github_pat";
const API: &str = "https://api.github.com";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhUser {
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhRepo {
    pub id: i64,
    pub full_name: String,
    pub private: bool,
    pub description: Option<String>,
    pub open_issues: i64,
    pub updated_at: Option<String>,
    pub html_url: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GhIssue {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub author: String,
    pub labels: Vec<String>,
    pub updated_at: Option<String>,
    pub html_url: String,
}

#[derive(Serialize)]
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

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("AutoAgents/0.1")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

async fn gh_get(token: &str, path: &str) -> Result<Value, String> {
    let resp = http()?
        .get(format!("{API}{path}"))
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| format!("网络错误: {e}"))?;
    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        return Err(match code {
            401 => "Token 无效或已过期（401）".into(),
            403 => "访问被拒绝（403），检查 Token 权限或 API 限额".into(),
            404 => "资源不存在（404），检查仓库名".into(),
            _ => format!("GitHub API 错误（{code}）"),
        });
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("解析响应失败: {e}"))
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

fn so(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(|x| x.to_string())
}

async fn fetch_user(token: &str) -> Result<GhUser, String> {
    let v = gh_get(token, "/user").await?;
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
    let user = fetch_user(&token).await?;
    state.secrets.set(PAT_ACCOUNT, &token)?;
    record(&state, "github.connected", "info", &user.login, "GitHub 已连接");
    Ok(user)
}

/// 查询连接状态：有保存的 Token 就拿它验证一次。
#[tauri::command]
pub async fn github_status(state: State<'_, Arc<AppState>>) -> Result<Option<GhUser>, String> {
    let Some(token) = state.secrets.get(PAT_ACCOUNT)? else {
        return Ok(None);
    };
    Ok(Some(fetch_user(&token).await?))
}

/// 断开连接：删除 Keychain 凭据。
#[tauri::command]
pub async fn github_disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.secrets.delete(PAT_ACCOUNT)?;
    record(&state, "github.disconnected", "info", "GitHub", "GitHub 已断开连接");
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
                })
                .collect()
        })
        .unwrap_or_default();
    repos.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(repos)
}

/// Issue 列表（state=all；issues API 混有 PR，过滤掉）。
#[tauri::command]
pub async fn github_issues(
    state: State<'_, Arc<AppState>>,
    owner: String,
    repo: String,
) -> Result<Vec<GhIssue>, String> {
    let token = token_of(&state)?;
    let path = format!("/repos/{owner}/{repo}/issues?state=all&per_page=30&sort=updated");
    let v = gh_get(&token, &path).await?;
    let list: Vec<GhIssue> = v
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|i| i.get("pull_request").is_none())
                .map(|i| GhIssue {
                    number: i["number"].as_i64().unwrap_or_default(),
                    title: s(i, "title"),
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
        .unwrap_or_default();
    Ok(list)
}

/// PR 列表（state=all）。
#[tauri::command]
pub async fn github_prs(
    state: State<'_, Arc<AppState>>,
    owner: String,
    repo: String,
) -> Result<Vec<GhPr>, String> {
    let token = token_of(&state)?;
    let path = format!("/repos/{owner}/{repo}/pulls?state=all&per_page=30&sort=updated&direction=desc");
    let v = gh_get(&token, &path).await?;
    Ok(v.as_array()
        .map(|arr| {
            arr.iter()
                .map(|p| GhPr {
                    number: p["number"].as_i64().unwrap_or_default(),
                    title: s(p, "title"),
                    state: s(p, "state"),
                    draft: p["draft"].as_bool().unwrap_or(false),
                    author: s(&p["user"], "login"),
                    head_ref: s(&p["head"], "ref"),
                    base_ref: s(&p["base"], "ref"),
                    updated_at: so(p, "updated_at"),
                    html_url: s(p, "html_url"),
                })
                .collect()
        })
        .unwrap_or_default())
}

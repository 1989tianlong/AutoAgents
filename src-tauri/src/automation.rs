//! 自动化规则：盯着 GitHub，条件满足就自动建工作流并启动。
//! 「人只负责决策和验收」的自动驾驶部分：
//!   - `issue-label`：仓库的 Issue 打上某个标签 → 自动跑修 Issue 流水线（或竞争模式）
//!   - `new-pr`：仓库来了新 PR → 自动跑 PR 审查流水线，汇总结论贴回 PR 评论
//!
//! 机器、Agent、仓库位置在建规则时配好；没配的项沿用这个仓库上次任务用过的设置。
//! 一条 Issue / 一个 PR 只自动处理一次（`rule_runs` 去重），一条规则一天最多触发
//! 十次，标签扫乱了也不会把工作流刷爆。

use crate::{AppState, events, github, nodes, pipeline, tasks, workflow};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tauri::State;

/// 检查间隔：自动化不需要秒级响应，两分钟看一眼 GitHub 足够
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(120);
/// 一条规则一天最多自动触发的工作流数
const MAX_RUNS_PER_DAY: i64 = 10;

// ---------------------------------------------------------------------------
// 数据
// ---------------------------------------------------------------------------

/// 规则的配置（rules.config 里那段 JSON）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct RuleConfig {
    /// issue-label：盯哪个标签
    label: String,
    /// issue-fix / race（issue-label 用）；pr-review 固定给 new-pr
    template: String,
    /// 各步骤用的 Agent（与模板步骤一一对应；空则用模板默认）
    adapters: Vec<String>,
    /// 各步骤用的模型（与模板步骤一一对应；空则用整个流的）
    models: Vec<String>,
    model: Option<String>,
    /// 任务步骤做完直接提交开 PR（验收就变成看 PR）
    auto_pr: bool,
    /// new-pr：审查完把汇总结论贴回 PR 评论
    comment: bool,
    /// 在哪台机器上跑（空 = 沿用这个仓库上次的）
    node_id: String,
    /// 仓库在那台机器上的位置（空 = 沿用这个仓库上次的）
    workdir: String,
}

struct Rule {
    id: i64,
    name: String,
    kind: String,
    repo: String,
    config: RuleConfig,
    enabled: bool,
}

fn row_to_rule(row: &rusqlite::Row) -> rusqlite::Result<Rule> {
    let config: String = row.get(4)?;
    Ok(Rule {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        repo: row.get(3)?,
        config: serde_json::from_str(&config).unwrap_or_default(),
        enabled: row.get::<_, i64>(5)? != 0,
    })
}

const RULE_COLS: &str = "id, name, kind, repo, config, enabled";

fn load_rules(db: &Connection, only_enabled: bool) -> Result<Vec<Rule>, String> {
    let sql = if only_enabled {
        format!("SELECT {RULE_COLS} FROM rules WHERE enabled = 1 ORDER BY id")
    } else {
        format!("SELECT {RULE_COLS} FROM rules ORDER BY id")
    };
    let mut stmt = db.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], row_to_rule)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn load_rule(db: &Connection, id: i64) -> Result<Rule, String> {
    db.query_row(
        &format!("SELECT {RULE_COLS} FROM rules WHERE id = ?1"),
        params![id],
        row_to_rule,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("没有这条规则：{id}"))
}

/// 界面上看到的规则（配置摊平，近 24 小时触发了多少次）
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    id: i64,
    name: String,
    kind: String,
    repo: String,
    label: String,
    template: String,
    adapters: Vec<String>,
    models: Vec<String>,
    model: Option<String>,
    auto_pr: bool,
    comment: bool,
    node_id: String,
    workdir: String,
    enabled: bool,
    runs_today: i64,
}

fn rule_view(db: &Connection, rule: &Rule) -> RuleView {
    let runs_today: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM rule_runs WHERE rule_id = ?1 AND created_at > datetime('now', '-1 day')",
            params![rule.id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    RuleView {
        id: rule.id,
        name: rule.name.clone(),
        kind: rule.kind.clone(),
        repo: rule.repo.clone(),
        label: rule.config.label.clone(),
        template: rule.config.template.clone(),
        adapters: rule.config.adapters.clone(),
        models: rule.config.models.clone(),
        model: rule.config.model.clone(),
        auto_pr: rule.config.auto_pr,
        comment: rule.config.comment,
        node_id: rule.config.node_id.clone(),
        workdir: rule.config.workdir.clone(),
        enabled: rule.enabled,
        runs_today,
    }
}

/// 一条规则的触发记录
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    id: i64,
    rule_id: i64,
    rule_name: String,
    key: String,
    workflow_id: Option<i64>,
    posted: bool,
    created_at: String,
}

// ---------------------------------------------------------------------------
// 命令（薄壳，逻辑都在内层函数里，测试直接调内层）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn automation_rules(state: State<'_, Arc<AppState>>) -> Result<Vec<RuleView>, String> {
    list_rules(state.inner())
}

pub fn list_rules(state: &Arc<AppState>) -> Result<Vec<RuleView>, String> {
    let db = state.db.lock().unwrap();
    Ok(load_rules(&db, false)?
        .iter()
        .map(|rule| rule_view(&db, rule))
        .collect())
}

#[tauri::command]
pub fn automation_runs(
    state: State<'_, Arc<AppState>>,
    limit: Option<i64>,
) -> Result<Vec<RunView>, String> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let db = state.db.lock().unwrap();
    let mut stmt = db
        .prepare(
            "SELECT rr.id, rr.rule_id, r.name, rr.key, rr.workflow_id, rr.posted, rr.created_at \
             FROM rule_runs rr JOIN rules r ON r.id = rr.rule_id \
             ORDER BY rr.id DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(RunView {
                id: row.get(0)?,
                rule_id: row.get(1)?,
                rule_name: row.get(2)?,
                key: row.get(3)?,
                workflow_id: row.get(4)?,
                posted: row.get::<_, i64>(5)? != 0,
                created_at: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

/// 建一条规则。机器与仓库位置可以不填——触发时沿用这个仓库上次任务的设置
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn automation_rule_create(
    state: State<'_, Arc<AppState>>,
    name: String,
    kind: String,
    repo: String,
    label: Option<String>,
    template: Option<String>,
    adapters: Option<Vec<String>>,
    models: Option<Vec<String>>,
    model: Option<String>,
    auto_pr: Option<bool>,
    comment: Option<bool>,
    node_id: Option<String>,
    workdir: Option<String>,
) -> Result<RuleView, String> {
    create_rule(
        state.inner(),
        Params {
            name,
            kind,
            repo,
            label,
            template,
            adapters,
            models,
            model,
            auto_pr,
            comment,
            node_id,
            workdir,
        },
    )
}

#[allow(clippy::too_many_arguments)]
struct Params {
    name: String,
    kind: String,
    repo: String,
    label: Option<String>,
    template: Option<String>,
    adapters: Option<Vec<String>>,
    models: Option<Vec<String>>,
    model: Option<String>,
    auto_pr: Option<bool>,
    comment: Option<bool>,
    node_id: Option<String>,
    workdir: Option<String>,
}

fn create_rule(state: &Arc<AppState>, p: Params) -> Result<RuleView, String> {
    let name = p.name.trim().to_string();
    let repo = p.repo.trim().to_string();
    if name.is_empty() {
        return Err("给规则起个名字".into());
    }
    pipeline::split_repo(&repo)?;
    let kind = p.kind.trim().to_string();
    let mut config = RuleConfig {
        adapters: p
            .adapters
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.trim().to_string())
            .collect(),
        models: p
            .models
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.trim().to_string())
            .collect(),
        model: p.model.filter(|m| !m.trim().is_empty()),
        auto_pr: p.auto_pr.unwrap_or(false),
        comment: p.comment.unwrap_or(true),
        node_id: nodes::normalize(
            p.node_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        ),
        workdir: p.workdir.unwrap_or_default().trim().to_string(),
        ..Default::default()
    };
    match kind.as_str() {
        "issue-label" => {
            config.label = p.label.unwrap_or_default().trim().to_string();
            if config.label.is_empty() {
                return Err("填上要盯的标签（Issue 打上它就自动开工）".into());
            }
            config.template = p
                .template
                .unwrap_or_else(|| "issue-fix".into())
                .trim()
                .to_string();
            if !matches!(config.template.as_str(), "issue-fix" | "race") {
                return Err("标签规则只能用「修 Issue 流水线」或「竞争模式」".into());
            }
        }
        "new-pr" => {
            config.template = "pr-review".into();
        }
        "pr-check-failed" => {
            // 不建工作流：检查红了直接让原来那个任务的 Agent 继续修
            config.template = "ci-fix".into();
        }
        _ => return Err("规则类型只有 issue-label（Issue 打标签）、new-pr（新 PR）和 pr-check-failed（检查失败自动修）".into()),
    }
    let db = state.db.lock().unwrap();
    db.execute(
        "INSERT INTO rules (name, kind, repo, config) VALUES (?1, ?2, ?3, ?4)",
        params![name, kind, repo, serde_json::to_string(&config).unwrap()],
    )
    .map_err(|e| e.to_string())?;
    let rule = load_rule(&db, db.last_insert_rowid())?;
    Ok(rule_view(&db, &rule))
}

/// 开关一条规则（关掉即停，触发记录保留，重开后没处理过的继续处理）
#[tauri::command]
pub fn automation_rule_set_enabled(
    state: State<'_, Arc<AppState>>,
    id: i64,
    enabled: bool,
) -> Result<RuleView, String> {
    set_rule_enabled(state.inner(), id, enabled)
}

fn set_rule_enabled(state: &Arc<AppState>, id: i64, enabled: bool) -> Result<RuleView, String> {
    let db = state.db.lock().unwrap();
    load_rule(&db, id)?;
    db.execute(
        "UPDATE rules SET enabled = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![enabled as i64, id],
    )
    .map_err(|e| e.to_string())?;
    let rule = load_rule(&db, id)?;
    Ok(rule_view(&db, &rule))
}

#[tauri::command]
pub fn automation_rule_delete(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    delete_rule(state.inner(), id)
}

fn delete_rule(state: &Arc<AppState>, id: i64) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    load_rule(&db, id)?;
    db.execute("DELETE FROM rules WHERE id = ?1", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 立刻检查一遍（平时两分钟一次；建好规则想马上生效就点它）。返回这次触发了几条
#[tauri::command]
pub async fn automation_scan(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    Ok(tick(state.inner()).await)
}

// ---------------------------------------------------------------------------
// 引擎
// ---------------------------------------------------------------------------

pub fn start_polling(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            tick(&state).await;
        }
    });
}

/// 检查一遍所有启用的规则。单条规则出错只记事件，不拖垮别的规则
pub async fn tick(state: &Arc<AppState>) -> usize {
    let Ok(token) = github::token_of(state) else {
        return 0; // 还没连 GitHub：没有可盯的
    };
    let rules = match load_rules(&state.db.lock().unwrap(), true) {
        Ok(rules) => rules,
        Err(_) => return 0,
    };
    let mut triggered = 0;
    for rule in rules {
        let outcome = match rule.kind.as_str() {
            "issue-label" => trigger_issues(state, &token, &rule).await,
            "new-pr" => trigger_prs(state, &token, &rule).await,
            "pr-check-failed" => trigger_check_fixes(state, &rule).await,
            other => Err(format!("不认识的规则类型：{other}")),
        };
        match outcome {
            Ok(n) => triggered += n,
            Err(e) => {
                let db = state.db.lock().unwrap();
                events::record(
                    &db,
                    "automation.error",
                    "warning",
                    &rule.name,
                    &format!("规则这次没跑成，下一轮会再试：{e}"),
                    None,
                );
            }
        }
    }
    triggered + post_pending_comments(state, &token).await
}

/// 触发时用的机器与仓库位置：规则里配了就用规则的，没配就沿用这个仓库上次任务
fn settings_for(
    state: &Arc<AppState>,
    rule: &Rule,
) -> Result<(String, String, Option<String>), String> {
    if !rule.config.workdir.is_empty() {
        let node = if rule.config.node_id.is_empty() {
            nodes::normalize(None)
        } else {
            rule.config.node_id.clone()
        };
        return Ok((node, rule.config.workdir.clone(), rule.config.model.clone()));
    }
    let previous = {
        let db = state.db.lock().unwrap();
        crate::deploy::previous_settings(&db, &rule.repo)
    };
    match previous {
        Some(opts) => {
            let node = if rule.config.node_id.is_empty() {
                opts.node_id
            } else {
                rule.config.node_id.clone()
            };
            Ok((node, opts.workdir, rule.config.model.clone().or(opts.model)))
        }
        None => Err(
            "这个仓库还没跑过任务，不知道仓库在哪台机器的哪个目录；在规则里填上「仓库位置」就好"
                .into(),
        ),
    }
}

/// 这条规则 24 小时内还能不能再触发；能就占住这个键（并发 / 重复都不会 double）
fn reserve(db: &Connection, rule: &Rule, key: &str) -> Result<bool, String> {
    let runs_today: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM rule_runs WHERE rule_id = ?1 AND created_at > datetime('now', '-1 day')",
            params![rule.id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if runs_today >= MAX_RUNS_PER_DAY {
        events::record(
            db,
            "automation.error",
            "warning",
            &rule.name,
            &format!("一天最多自动触发 {MAX_RUNS_PER_DAY} 次，剩下的明天再说：{key}"),
            None,
        );
        return Ok(false);
    }
    let inserted = db
        .execute(
            "INSERT OR IGNORE INTO rule_runs (rule_id, key) VALUES (?1, ?2)",
            params![rule.id, key],
        )
        .map_err(|e| e.to_string())?;
    Ok(inserted > 0)
}

/// 建好工作流并把触发记录指过去。建不成就把占住的键还回去（下一轮重试）
async fn launch(
    state: &Arc<AppState>,
    rule: &Rule,
    key: &str,
    workflow_id: Result<i64, String>,
) -> Result<(), String> {
    let id = match workflow_id {
        Ok(id) => id,
        Err(e) => {
            let db = state.db.lock().unwrap();
            let _ = db.execute(
                "DELETE FROM rule_runs WHERE rule_id = ?1 AND key = ?2",
                params![rule.id, key],
            );
            return Err(e);
        }
    };
    {
        let db = state.db.lock().unwrap();
        db.execute(
            "UPDATE rule_runs SET workflow_id = ?1 WHERE rule_id = ?2 AND key = ?3",
            params![id, rule.id, key],
        )
        .map_err(|e| e.to_string())?;
        events::record(
            &db,
            "automation.triggered",
            "info",
            &rule.name,
            &format!("规则触发：{key} → 工作流 #{id}"),
            Some(&format!("#/workflows?w={id}")),
        );
    }
    if let Err(e) = workflow::start(state, id).await {
        let db = state.db.lock().unwrap();
        events::record(
            &db,
            "automation.error",
            "warning",
            &rule.name,
            &format!("工作流 #{id} 建好了但没能自动启动（可以在工作流页手动启动）：{e}"),
            Some(&format!("#/workflows?w={id}")),
        );
    }
    Ok(())
}

async fn trigger_issues(state: &Arc<AppState>, token: &str, rule: &Rule) -> Result<usize, String> {
    let (owner, repo) = pipeline::split_repo(&rule.repo)?;
    let issues = github::issues_list(
        state,
        token,
        &owner,
        &repo,
        "state=open&per_page=30&sort=updated",
    )
    .await?;
    let mut triggered = 0;
    for issue in issues
        .iter()
        .filter(|i| i.state == "open" && i.labels.iter().any(|l| l == &rule.config.label))
    {
        let key = format!("issue:{}", issue.number);
        if !reserve(&state.db.lock().unwrap(), rule, &key)? {
            continue;
        }
        let result = match settings_for(state, rule) {
            Ok((node, workdir, model)) => workflow::create(
                state,
                &rule.config.template,
                &rule.repo,
                Some(issue.number),
                Some(issue.title.clone()).filter(|t| !t.is_empty()),
                Some(issue.html_url.clone()).filter(|u| !u.is_empty()),
                None,
                None,
                &node,
                &workdir,
                model,
                rule.config.auto_pr,
                rule.config.adapters.clone(),
                rule.config.models.clone(),
            )
            .await
            .map(|wf| wf.id),
            Err(e) => Err(e),
        };
        launch(state, rule, &key, result).await?;
        triggered += 1;
    }
    Ok(triggered)
}

async fn trigger_prs(state: &Arc<AppState>, token: &str, rule: &Rule) -> Result<usize, String> {
    let (owner, repo) = pipeline::split_repo(&rule.repo)?;
    let prs = github::prs_list(
        state,
        token,
        &owner,
        &repo,
        "state=open&per_page=30&sort=updated&direction=desc",
    )
    .await?;
    let mut triggered = 0;
    // 草稿 PR 作者还在改，不抢着审
    for pr in prs.iter().filter(|p| p.state == "open" && !p.draft) {
        let key = format!("pr:{}", pr.number);
        if !reserve(&state.db.lock().unwrap(), rule, &key)? {
            continue;
        }
        let result = match settings_for(state, rule) {
            Ok((node, workdir, model)) => workflow::create(
                state,
                "pr-review",
                &rule.repo,
                None,
                None,
                None,
                Some(pr.number),
                None,
                &node,
                &workdir,
                model,
                false,
                rule.config.adapters.clone(),
                rule.config.models.clone(),
            )
            .await
            .map(|wf| wf.id),
            Err(e) => Err(e),
        };
        launch(state, rule, &key, result).await?;
        triggered += 1;
    }
    Ok(triggered)
}

/// PR 的检查红了：直接让原来那个任务的 Agent 在同一个会话里继续修（不建新工作流）。
/// 按任务 + 推送的头一个提交去重——修完推上来的新提交要是还红，会再修（有每日上限兜底）
async fn trigger_check_fixes(state: &Arc<AppState>, rule: &Rule) -> Result<usize, String> {
    // 只扫任务流水线盯着的 PR，不去 GitHub 列全部 PR：别的来源的 PR 归它的作者管
    let reviewing = tasks::in_status(&state.db.lock().unwrap(), "reviewing")?;
    let mut triggered = 0;
    for task in reviewing
        .iter()
        .filter(|t| t.repo.as_deref() == Some(rule.repo.as_str()))
        .filter(|t| t.pr_number.is_some() && t.session_id.is_some())
    {
        let red = task
            .checks
            .as_ref()
            .and_then(Value::as_array)
            .is_some_and(|list| list.iter().any(|c| c["state"] == "failure"));
        if !red {
            continue;
        }
        let Some(head) = task.head_sha.clone().filter(|s| !s.is_empty()) else {
            continue;
        };
        let key = format!("check:{}:{}", task.id, head);
        if !reserve(&state.db.lock().unwrap(), rule, &key)? {
            continue;
        }
        match pipeline::fix(state, task.id).await {
            Ok(()) => {
                let db = state.db.lock().unwrap();
                events::record(
                    &db,
                    "automation.triggered",
                    "info",
                    &rule.name,
                    &format!(
                        "PR #{} 的检查红了，已让原来那个 Agent 修（任务 #{}）",
                        task.pr_number.unwrap_or_default(),
                        task.id
                    ),
                    Some(&format!("#/tasks?t={}", task.id)),
                );
                triggered += 1;
            }
            Err(e) => {
                // 发不出去（上一轮还没停之类）：把键还回去，下一轮再试
                let db = state.db.lock().unwrap();
                let _ = db.execute(
                    "DELETE FROM rule_runs WHERE rule_id = ?1 AND key = ?2",
                    params![rule.id, &key],
                );
                events::record(
                    &db,
                    "automation.error",
                    "warning",
                    &rule.name,
                    &format!(
                        "任务 #{} 的检查红了，但这次没能让 Agent 修，下一轮再试：{e}",
                        task.id
                    ),
                    Some(&format!("#/tasks?t={}", task.id)),
                );
            }
        }
    }
    Ok(triggered)
}

/// PR 审查流水线跑完了、结论还没贴回去的：把「汇总结论」步骤的产出贴成 PR 评论。
/// 贴过（或贴不成）都打上标记，不会反复刷
async fn post_pending_comments(state: &Arc<AppState>, token: &str) -> usize {
    let pending: Vec<(i64, i64, String, i64)> = {
        let db = state.db.lock().unwrap();
        let Ok(mut stmt) = db.prepare(
            "SELECT rr.id, wf.id, wf.repo, wf.pr_number \
             FROM rule_runs rr \
             JOIN rules r ON r.id = rr.rule_id \
             JOIN workflows wf ON wf.id = rr.workflow_id \
             WHERE r.kind = 'new-pr' AND r.enabled = 1 AND rr.posted = 0 \
               AND wf.template = 'pr-review' AND wf.status = 'done' AND wf.pr_number IS NOT NULL",
        ) else {
            return 0;
        };
        stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    };
    let mut posted = 0;
    for (run_id, wf_id, repo, pr_number) in pending {
        let summary: Option<String> = {
            let db = state.db.lock().unwrap();
            db.query_row(
                "SELECT outcome FROM workflow_steps WHERE workflow_id = ?1 AND kind = 'session' AND outcome IS NOT NULL \
                 ORDER BY seq DESC LIMIT 1",
                params![wf_id],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()
        };
        let (owner, repo_name) = match pipeline::split_repo(&repo) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let outcome = if let Some(text) = summary.filter(|t| !t.trim().is_empty()) {
            github::comment_pr(
                state,
                token,
                &owner,
                &repo_name,
                pr_number,
                &format!(
                    "## 🤖 AutoAgents 审查结论\n\n{text}\n\n*来自 AutoAgents 的 PR 审查流水线：两位 Agent 并行交叉审查、第三人汇总。要它重审就在应用里重跑。*"
                ),
            )
            .await
        } else {
            Err("审查流水线没有产出结论".to_string())
        };
        let db = state.db.lock().unwrap();
        let _ = db.execute(
            "UPDATE rule_runs SET posted = 1 WHERE id = ?1",
            params![run_id],
        );
        match outcome {
            Ok(()) => {
                posted += 1;
                events::record(
                    &db,
                    "automation.commented",
                    "info",
                    &repo,
                    &format!("PR #{pr_number} 的自动审查结论已贴成评论"),
                    Some(&format!("#/workflows?w={wf_id}")),
                );
            }
            Err(e) => {
                events::record(
                    &db,
                    "automation.error",
                    "warning",
                    &repo,
                    &format!("PR #{pr_number} 的审查结论没贴成：{e}"),
                    Some(&format!("#/workflows?w={wf_id}")),
                );
            }
        }
    }
    posted
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use crate::testutil::{
        Req, TestState, fake_script, read_request, respond, setup_repo, test_state,
    };
    use autoagents_core::runner::BinaryResolver;
    use autoagents_core::service::{Service, ServiceOptions};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// 假的 GitHub API：自动化会调的接口（Issue / PR 列表、PR diff、评论）
    struct FakeGithub {
        addr: String,
        calls: Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
    }

    async fn start_github() -> FakeGithub {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let fake = FakeGithub {
            addr,
            calls: Arc::default(),
        };
        let calls = Arc::clone(&fake.calls);
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let calls = Arc::clone(&calls);
                tokio::spawn(async move {
                    let Req {
                        method, path, body, ..
                    } = read_request(&mut stream).await;
                    calls
                        .lock()
                        .unwrap()
                        .push((method.clone(), path.clone(), body.clone()));
                    macro_rules! json {
                        ($text:expr) => {
                            respond(&mut stream, "200 OK", "application/json", $text)
                        };
                    }
                    let route = path.split('?').next().unwrap_or("").to_string();
                    match (method.as_str(), route.as_str()) {
                        ("GET", "/repos/o/r") => json!(r#"{"default_branch": "main"}"#).await,
                        // Issue 列表：#7 带标签，#8 没有
                        ("GET", "/repos/o/r/issues") => json!(
                            r#"[{"number": 7, "title": "加一句问候", "state": "open", "labels": [{"name": "agent-fix"}], "html_url": "https://github.test/o/r/issues/7"},
                                {"number": 8, "title": "没标签的", "state": "open", "labels": [], "html_url": "https://github.test/o/r/issues/8"}]"#
                        )
                        .await,
                        ("GET", "/repos/o/r/issues/7") => json!(
                            r#"{"number": 7, "title": "加一句问候", "body": "在 README 里加一句问候。", "html_url": "https://github.test/o/r/issues/7"}"#
                        )
                        .await,
                        // PR 列表：#5 开着，#6 是草稿
                        ("GET", "/repos/o/r/pulls") => json!(
                            r#"[{"number": 5, "title": "改首页", "state": "open", "draft": false, "user": {"login": "me"}, "head": {"ref": "feat"}, "base": {"ref": "main"}, "html_url": "https://github.test/o/r/pull/5"},
                                {"number": 6, "title": "草稿", "state": "open", "draft": true, "user": {"login": "me"}, "head": {"ref": "wip"}, "base": {"ref": "main"}, "html_url": "https://github.test/o/r/pull/6"}]"#
                        )
                        .await,
                        // PR 的 diff（纯文本接口）
                        ("GET", "/repos/o/r/pulls/5") => respond(
                            &mut stream,
                            "200 OK",
                            "text/plain",
                            "diff --git a/app.txt b/app.txt\n--- a/app.txt\n+++ b/app.txt\n@@ -1 +1,2 @@\n hello\n+world\n",
                        )
                        .await,
                        // 贴审查结论的评论
                        ("POST", "/repos/o/r/issues/5/comments") => json!(r#"{"id": 99}"#).await,
                        _ => json!(r#"{"message": "Not Found"}"#).await,
                    }
                });
            }
        });
        fake
    }

    /// 只回一句话、不改文件的 Agent（claude / grok 的 stream-json 协议）
    fn talker_agent(reply: &str) -> String {
        format!(r#"printf '%s\n' '{{"type":"result","is_error":false,"result":"{reply}"}}'"#)
    }

    /// 写一个文件再回一句话的 Agent（任务步骤要求真的有改动）
    fn writer_agent(file: &str, reply: &str) -> String {
        format!(
            r#"echo fixed > {file}
printf '%s\n' '{{"type":"result","is_error":false,"result":"{reply}"}}'"#
        )
    }

    /// codex 是另一套协议：整条 agent_message + task_complete
    fn codex_talker_agent(reply: &str) -> String {
        format!(
            r#"printf '%s\n' '{{"type":"agent_message","message":"{reply}"}}'
printf '%s\n' '{{"type":"task_complete"}}'"#
        )
    }

    fn agent_service(pairs: Vec<(&str, &str)>) -> Arc<Service> {
        let map: HashMap<String, PathBuf> = pairs
            .into_iter()
            .map(|(name, script)| (name.to_string(), fake_script(name, script)))
            .collect();
        let resolve: BinaryResolver = Arc::new(move |name: &str| map.get(name).cloned());
        Service::new(ServiceOptions {
            resolve: Some(resolve),
            ..Default::default()
        })
    }

    /// 配好假 GitHub 与可用工作目录的测试状态
    async fn rig(pairs: Vec<(&str, &str)>) -> (FakeGithub, TestState, PathBuf) {
        let github = start_github().await;
        let tmp = tempfile::tempdir().unwrap();
        let workdir = setup_repo(tmp.path());
        let ts = test_state(agent_service(pairs));
        *ts.state.gh_api.lock().unwrap() = github.addr.clone();
        ts.secrets.set("github_pat", "test-token").unwrap();
        std::mem::forget(tmp);
        (github, ts, workdir)
    }

    fn make_rule(state: &Arc<AppState>, kind: &str, workdir: &str, adapters: &[&str]) -> RuleView {
        create_rule(
            state,
            Params {
                models: None,
                name: "盯着 o/r".into(),
                kind: kind.into(),
                repo: "o/r".into(),
                label: Some("agent-fix".into()),
                template: None,
                adapters: Some(adapters.iter().map(|s| s.to_string()).collect()),
                model: None,
                auto_pr: Some(false),
                comment: None,
                node_id: None,
                workdir: Some(workdir.into()),
            },
        )
        .unwrap()
    }

    fn param(name: &str, label: Option<&str>, workdir: Option<&str>) -> Params {
        Params {
            models: None,
            name: name.into(),
            kind: "issue-label".into(),
            repo: "o/r".into(),
            label: label.map(Into::into),
            template: None,
            adapters: None,
            model: None,
            auto_pr: None,
            comment: None,
            node_id: None,
            workdir: workdir.map(Into::into),
        }
    }

    /// 推工作流引擎直到终态，返回 (状态, 步骤概览)
    async fn wait_wf(
        state: &Arc<AppState>,
        id: i64,
    ) -> (String, Vec<(String, String, Option<String>)>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
        loop {
            workflow::tick(state).await;
            let snapshot = {
                let db = state.db.lock().unwrap();
                let status: String = db
                    .query_row(
                        "SELECT status FROM workflows WHERE id = ?1",
                        params![id],
                        |r| r.get(0),
                    )
                    .unwrap();
                let steps: Vec<(String, String, Option<String>)> = db
                    .prepare("SELECT name, status, failure FROM workflow_steps WHERE workflow_id = ?1 ORDER BY seq")
                    .unwrap()
                    .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .unwrap()
                    .flatten()
                    .collect();
                (status, steps)
            };
            if matches!(snapshot.0.as_str(), "done" | "failed" | "cancelled") {
                return snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "工作流一直停在 {}（{:?}）",
                snapshot.0,
                snapshot.1
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    fn event_exists(state: &Arc<AppState>, needle: &str) -> bool {
        state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT id FROM events WHERE message LIKE ?1 LIMIT 1",
                params![format!("%{needle}%")],
                |row| row.get::<_, i64>(0),
            )
            .is_ok()
    }

    /// Issue 打上标签 → 自动建工作流并启动跑完三步；再扫一遍不会重复建
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn issue_label_triggers_once() {
        let (_gh, ts, workdir) = rig(vec![
            ("claude-cli", &writer_agent("fix.txt", "修好了")),
            ("codex-cli", &codex_talker_agent("没有明显问题")),
            ("grok-cli", &talker_agent("变更说明：加了问候")),
        ])
        .await;
        let rule = make_rule(&ts.state, "issue-label", &workdir.to_string_lossy(), &[]);
        assert!(rule.enabled);
        assert_eq!(rule.template, "issue-fix");

        assert_eq!(tick(&ts.state).await, 1, "带标签的 Issue #7 触发了一条");
        let (status, steps) = wait_wf(&ts.state, 1).await;
        assert_eq!(status, "done", "{steps:?}");
        assert!(steps.iter().all(|(_, s, _)| s == "done"), "{steps:?}");
        // 工作流记录的是那个 Issue
        let (issue, name): (i64, String) = ts
            .state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT issue_number, name FROM workflows WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(issue, 7);
        assert!(name.starts_with("修 Issue 流水线"), "{name}");

        // 第二遍：同一个 Issue 不重复处理；没标签的 #8 不理
        assert_eq!(tick(&ts.state).await, 0);
        let count: i64 = ts
            .state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM workflows", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert!(event_exists(&ts.state, "规则触发：issue:7"));
    }

    /// 新 PR 自动审查，跑完把汇总结论贴成 PR 评论；草稿 PR 不动
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn new_pr_gets_reviewed_and_commented() {
        let (gh, ts, workdir) = rig(vec![
            ("claude-cli", &talker_agent("第一审：没有阻塞问题")),
            ("codex-cli", &codex_talker_agent("第二审：同意")),
            ("grok-cli", &talker_agent("汇总：建议通过")),
        ])
        .await;
        make_rule(
            &ts.state,
            "new-pr",
            &workdir.to_string_lossy(),
            &["claude-cli", "codex-cli", "grok-cli"],
        );

        assert_eq!(tick(&ts.state).await, 1, "PR #5 触发审查；草稿 #6 不算");
        let (status, steps) = wait_wf(&ts.state, 1).await;
        assert_eq!(status, "done", "{steps:?}");

        // 跑完之后再扫一遍：结论贴回 PR 评论
        assert_eq!(tick(&ts.state).await, 1, "贴评论也算一次动作");
        {
            let calls = gh.calls.lock().unwrap();
            let (_, _, body) = calls
                .iter()
                .find(|(method, path, _)| {
                    method == "POST" && path.starts_with("/repos/o/r/issues/5/comments")
                })
                .expect("没有发出评论");
            let v: serde_json::Value = serde_json::from_str(body).unwrap();
            let text = v["body"].as_str().unwrap();
            assert!(text.contains("汇总：建议通过"), "{text}");
            assert!(text.contains("审查结论"), "{text}");
        }
        // 再扫不重复贴
        assert_eq!(tick(&ts.state).await, 0);
        assert!(event_exists(&ts.state, "已贴成评论"));
    }

    /// 没配仓库位置、仓库也没跑过任务：不建工作流，事件里说清楚缺什么，也不占去重键
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_workdir_reports_instead_of_failing() {
        let (_gh, ts, _workdir) = rig(vec![]).await;
        make_rule(&ts.state, "issue-label", "", &[]);
        assert_eq!(tick(&ts.state).await, 0);
        assert!(event_exists(&ts.state, "不知道仓库在哪台机器的哪个目录"));
        let runs: i64 = ts
            .state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM rule_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs, 0, "没占键：配好位置之后同一批 Issue 还能触发");
    }

    /// PR 的检查红了：自动让原来那个任务的 Agent 修；同一提交不重复修
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn red_checks_get_fixed_automatically() {
        // 慢一点的 Agent：断言时这一轮还在跑，任务停在「正在修复」
        let (_gh, ts, _workdir) = rig(vec![(
            "claude-cli",
            "sleep 2\nprintf '%s\\n' '{\"type\":\"result\",\"is_error\":false,\"result\":\"修好了\"}'",
        )])
        .await;
        make_rule(&ts.state, "pr-check-failed", "", &[]);
        // 一个 PR 已开、检查红着的任务（带着它自己的会话）
        let task_id = {
            let db = ts.state.db.lock().unwrap();
            db.execute(
                "INSERT INTO sessions (adapter, title, workdir) VALUES ('claude-cli', '修检查', ?1)",
                params![_workdir.to_string_lossy().to_string()],
            )
            .unwrap();
            let session = db.last_insert_rowid();
            db.execute(
                "INSERT INTO tasks (title, repo, status, session_id, pr_number, head_sha, checks, node_id, adapter, workdir) \
                 VALUES ('修首页', 'o/r', 'reviewing', ?1, 5, 'abc123', ?2, 'local', 'claude-cli', ?3)",
                params![
                    session,
                    r#"[{"name": "ci", "state": "failure"}, {"name": "lint", "state": "success"}]"#,
                    _workdir.to_string_lossy().to_string(),
                ],
            )
            .unwrap();
            db.last_insert_rowid()
        };

        assert_eq!(tick(&ts.state).await, 1, "红检查触发了一次自动修");
        let (status, detail): (String, Option<String>) = ts
            .state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT status, detail FROM tasks WHERE id = ?1",
                params![task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(status, "running", "{detail:?}");
        // 发给 Agent 的最后一条提示词就是把检查的问题交过去修
        let prompt: String = ts
            .state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT m.content FROM messages m JOIN tasks t ON t.session_id = m.session_id \
                 WHERE t.id = ?1 AND m.role = 'user' ORDER BY m.id DESC LIMIT 1",
                params![task_id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(prompt.contains("检查没有通过"), "{prompt}");
        assert!(event_exists(&ts.state, "检查红了，已让原来那个 Agent 修"));
        let runs: Vec<String> = ts
            .state
            .db
            .lock()
            .unwrap()
            .prepare("SELECT key FROM rule_runs")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(runs, vec![format!("check:{task_id}:abc123")]);

        // 同一个提交再扫不重复修（任务也已经不在 reviewing 了）
        assert_eq!(tick(&ts.state).await, 0);
    }

    /// 规则的增删改与参数校验；关掉的规则不再触发
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rules_crud_and_disabled_does_not_fire() {
        let (_gh, ts, workdir) = rig(vec![]).await;
        let rule = make_rule(&ts.state, "issue-label", &workdir.to_string_lossy(), &[]);
        set_rule_enabled(&ts.state, rule.id, false).unwrap();
        assert!(!list_rules(&ts.state).unwrap()[0].enabled);
        assert_eq!(tick(&ts.state).await, 0);

        assert!(
            create_rule(&ts.state, param("", Some("x"), None)).is_err(),
            "没名字"
        );
        assert!(
            create_rule(&ts.state, param("坏", None, None)).is_err(),
            "标签规则没填标签"
        );
        assert!(
            create_rule(
                &ts.state,
                Params {
                    models: None,
                    kind: "magic".into(),
                    ..param("坏类型", Some("x"), None)
                }
            )
            .is_err(),
            "不认识的类型"
        );

        delete_rule(&ts.state, rule.id).unwrap();
        assert!(list_rules(&ts.state).unwrap().is_empty());
    }
}

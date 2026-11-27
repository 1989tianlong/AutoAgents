//! 工作流编排：把多个步骤串成一张有向无环图，让几个 Agent 在不同机器上按依赖协同。
//!
//! 三种步骤：
//! - `task`    走任务流水线：从 Issue / 需求说明建任务，在独立工作树里让 Agent 实现
//! - `session` 一段纯会话（交叉审查、写变更说明）：把依赖步骤的产物拼进提示词发给另一个 Agent
//! - `gate`    人工门：停下来等人决策（竞争模式里在候选方案里挑一个）
//!
//! 引擎的推进方式与部署、任务一致：后台每 10 秒 tick 一次，命令执行完也立刻推一把。
//! 每一步的动态都记进活动流，链回工作流页。
//!
//! 依赖写成 seq 的数组（`["2", "?3"]`）：带 `?` 前缀是软依赖，上游失败也算解锁——
//! 竞争模式的评审门不会因为其中一路失败而打不开。

use crate::AppState;
use crate::chat;
use crate::events;
use crate::github;
use crate::nodes;
use crate::pipeline::{self, StartOptions};
use crate::tasks::{self, Task};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::State;

/// 工作流的状态：pending 建好没启动；waiting 只剩人工门没处理
pub const STATUSES: &[&str] = &[
    "pending",
    "running",
    "waiting",
    "done",
    "failed",
    "cancelled",
];

/// 会话步骤产出的上限（字符）
const OUTCOME_LIMIT: usize = 4_000;
/// 拼进提示词的 diff 总量上限（字符）
const DIFF_LIMIT: usize = 60_000;
/// 单个文件的 diff 上限
const FILE_DIFF_LIMIT: usize = 20_000;
/// 每个依赖的审查结论上限
const REVIEW_LIMIT: usize = 8_000;

// ---------------------------------------------------------------------------
// 模板
// ---------------------------------------------------------------------------

pub struct TemplateStep {
    pub name: &'static str,
    /// `task` / `session` / `gate`
    pub kind: &'static str,
    /// 依赖的步骤 seq；`?` 前缀表示软依赖
    pub deps: &'static [&'static str],
    pub adapter: Option<&'static str>,
    /// session 步骤的提示词（带占位符）
    pub prompt: Option<&'static str>,
    /// session 步骤的改动从哪来：工作树 / PR 的 diff / 不带
    pub diff_source: &'static str,
}

pub struct Template {
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    /// 创建时要给 PR 号
    pub needs_pr: bool,
    /// 不用编排、已经做在别的页面里的模板（界面上显示跳转链接，不开放创建）
    pub live: Option<&'static str>,
    pub steps: &'static [TemplateStep],
}

const ISSUE_FIX_REVIEW: &str = "你在协助开发「{repo}」。有人按需求「{task}」改完了代码，改动如下：\n\n{diff}\n\n请作为严格的代码审查者检查这次改动：正确性、边界情况、测试覆盖、安全隐患。只输出问题清单（按严重程度排序，没有问题就明说）和一句话总评，不要修改任何文件。";

const ISSUE_FIX_NOTES: &str = "为「{repo}」的这次改动写一段变更说明（给发布公告用）。\n\n需求背景：{task}\n\n改动内容：\n{diff}\n\n审查意见：\n{reviews}\n\n直接输出 Markdown 正文：做了什么、为什么、影响范围、注意事项。不要修改任何文件。";

const PR_REVIEW: &str = "请审查「{repo}」的 PR #{pr}。\n\nPR 的完整 diff：\n\n{diff}\n\n从正确性、边界情况、测试、安全四个角度检查，输出：问题清单（按严重程度排序）与一句话结论（通过 / 需修改 / 拒绝）。不要修改任何文件。";

const PR_REVIEW_SUM: &str = "两位审查者检查了「{repo}」的 PR #{pr}，意见如下。\n\n{reviews}\n\n请汇总一份结论：大家一致同意的问题、有分歧的地方、最终建议（通过 / 需修改 / 拒绝）。直接输出 Markdown。";

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "issue-fix",
        name: "修 Issue 流水线",
        desc: "一个 Agent 分析修复，第二个交叉审查，第三个撰写变更说明",
        needs_pr: false,
        live: None,
        steps: &[
            TemplateStep {
                name: "分析修复",
                kind: "task",
                deps: &[],
                adapter: Some("claude-cli"),
                prompt: None,
                diff_source: "none",
            },
            TemplateStep {
                name: "交叉审查",
                kind: "session",
                deps: &["1"],
                adapter: Some("codex-cli"),
                prompt: Some(ISSUE_FIX_REVIEW),
                diff_source: "worktree",
            },
            TemplateStep {
                name: "撰写变更说明",
                kind: "session",
                deps: &["2"],
                adapter: Some("grok-cli"),
                prompt: Some(ISSUE_FIX_NOTES),
                diff_source: "worktree",
            },
        ],
    },
    Template {
        id: "pr-review",
        name: "PR 审查流水线",
        desc: "两位 Agent 并行交叉审查同一个 PR，第三人汇总结论",
        needs_pr: true,
        live: None,
        steps: &[
            TemplateStep {
                name: "审查 · Agent 一",
                kind: "session",
                deps: &[],
                adapter: Some("claude-cli"),
                prompt: Some(PR_REVIEW),
                diff_source: "pr",
            },
            TemplateStep {
                name: "审查 · Agent 二",
                kind: "session",
                deps: &[],
                adapter: Some("codex-cli"),
                prompt: Some(PR_REVIEW),
                diff_source: "pr",
            },
            TemplateStep {
                name: "汇总结论",
                kind: "session",
                deps: &["1", "2"],
                adapter: None,
                prompt: Some(PR_REVIEW_SUM),
                diff_source: "pr",
            },
        ],
    },
    Template {
        id: "race",
        name: "竞争模式",
        desc: "两个 Agent 并行解决同一 Issue（各自分支），人工评审择优",
        needs_pr: false,
        live: None,
        steps: &[
            TemplateStep {
                name: "方案 A",
                kind: "task",
                deps: &[],
                adapter: Some("claude-cli"),
                prompt: None,
                diff_source: "none",
            },
            TemplateStep {
                name: "方案 B",
                kind: "task",
                deps: &[],
                adapter: Some("codex-cli"),
                prompt: None,
                diff_source: "none",
            },
            TemplateStep {
                name: "人工评审",
                kind: "gate",
                deps: &["?1", "?2"],
                adapter: None,
                prompt: None,
                diff_source: "none",
            },
        ],
    },
    Template {
        id: "publish-guard",
        name: "发布守护流水线",
        desc: "生产部署 → 健康检查 → 失败自动建修复任务",
        needs_pr: false,
        live: Some("/deploy"),
        steps: &[],
    },
];

fn template_of(id: &str) -> Result<&'static Template, String> {
    TEMPLATES
        .iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("没有这个模板：{id}"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateStepView {
    pub name: String,
    pub kind: String,
    pub deps: Vec<String>,
    pub adapter: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateView {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub needs_pr: bool,
    pub live: Option<String>,
    pub steps: Vec<TemplateStepView>,
}

#[tauri::command]
pub fn workflow_templates() -> Vec<TemplateView> {
    TEMPLATES
        .iter()
        .map(|t| TemplateView {
            id: t.id.into(),
            name: t.name.into(),
            desc: t.desc.into(),
            needs_pr: t.needs_pr,
            live: t.live.map(Into::into),
            steps: t
                .steps
                .iter()
                .map(|s| TemplateStepView {
                    name: s.name.into(),
                    kind: s.kind.into(),
                    deps: s.deps.iter().map(|d| (*d).into()).collect(),
                    adapter: s.adapter.map(Into::into),
                })
                .collect(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 行与视图
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct WfRow {
    id: i64,
    name: String,
    template: Option<String>,
    repo: String,
    issue_number: Option<i64>,
    issue_title: Option<String>,
    instructions: Option<String>,
    pr_number: Option<i64>,
    status: String,
    detail: Option<String>,
}

const WF_COLS: &str =
    "id, name, template, repo, issue_number, issue_title, instructions, pr_number, status, detail";

fn row_to_wf(row: &rusqlite::Row) -> rusqlite::Result<WfRow> {
    Ok(WfRow {
        id: row.get(0)?,
        name: row.get(1)?,
        template: row.get(2)?,
        repo: row.get(3)?,
        issue_number: row.get(4)?,
        issue_title: row.get(5)?,
        instructions: row.get(6)?,
        pr_number: row.get(7)?,
        status: row.get(8)?,
        detail: row.get(9)?,
    })
}

fn load_wf(db: &Connection, id: i64) -> Result<WfRow, String> {
    db.query_row(
        &format!("SELECT {WF_COLS} FROM workflows WHERE id = ?1"),
        params![id],
        row_to_wf,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("工作流不存在：#{id}"))
}

#[derive(Clone, Debug)]
struct StepRow {
    id: i64,
    seq: i64,
    name: String,
    kind: String,
    depends_on: Vec<String>,
    config: Value,
    status: String,
    task_id: Option<i64>,
    session_id: Option<i64>,
    outcome: Option<String>,
    detail: Option<String>,
    failure: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
}

const STEP_COLS: &str = "id, seq, name, kind, depends_on, config, status, task_id, session_id, outcome, detail, failure, started_at, finished_at";

fn row_to_step(row: &rusqlite::Row) -> rusqlite::Result<StepRow> {
    let depends_on: String = row.get(4)?;
    let config: String = row.get(5)?;
    Ok(StepRow {
        id: row.get(0)?,
        seq: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        depends_on: serde_json::from_str(&depends_on).unwrap_or_default(),
        config: serde_json::from_str(&config).unwrap_or_else(|_| json!({})),
        status: row.get(6)?,
        task_id: row.get(7)?,
        session_id: row.get(8)?,
        outcome: row.get(9)?,
        detail: row.get(10)?,
        failure: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
    })
}

fn steps_of(db: &Connection, wf_id: i64) -> Result<Vec<StepRow>, String> {
    let mut stmt = db
        .prepare_cached(&format!(
            "SELECT {STEP_COLS} FROM workflow_steps WHERE workflow_id = ?1 ORDER BY seq"
        ))
        .map_err(|e| e.to_string())?;
    stmt.query_map(params![wf_id], row_to_step)
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepView {
    pub id: i64,
    pub seq: i64,
    pub name: String,
    pub kind: String,
    pub depends_on: Vec<String>,
    pub adapter: Option<String>,
    pub node_id: Option<String>,
    pub workdir: Option<String>,
    pub model: Option<String>,
    pub auto_pr: bool,
    pub prompt: Option<String>,
    pub status: String,
    pub task_id: Option<i64>,
    pub session_id: Option<i64>,
    pub outcome: Option<String>,
    pub detail: Option<String>,
    pub failure: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

impl StepRow {
    fn view(&self) -> StepView {
        StepView {
            id: self.id,
            seq: self.seq,
            name: self.name.clone(),
            kind: self.kind.clone(),
            depends_on: self.depends_on.clone(),
            adapter: self.config["adapter"].as_str().map(Into::into),
            node_id: self.config["node_id"].as_str().map(Into::into),
            workdir: self.config["workdir"].as_str().map(Into::into),
            model: self.config["model"].as_str().map(Into::into),
            auto_pr: self.config["auto_pr"].as_bool().unwrap_or(false),
            prompt: self.config["prompt"].as_str().map(Into::into),
            status: self.status.clone(),
            task_id: self.task_id,
            session_id: self.session_id,
            outcome: self.outcome.clone(),
            detail: self.detail.clone(),
            failure: self.failure.clone(),
            started_at: self.started_at.clone(),
            finished_at: self.finished_at.clone(),
        }
    }

    fn conf_str(&self, key: &str) -> String {
        self.config[key].as_str().unwrap_or_default().to_string()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowView {
    pub id: i64,
    pub name: String,
    pub template: Option<String>,
    pub repo: String,
    pub issue_number: Option<i64>,
    pub issue_title: Option<String>,
    pub instructions: Option<String>,
    pub pr_number: Option<i64>,
    pub status: String,
    pub detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub steps: Vec<StepView>,
}

fn view(db: &Connection, wf: &WfRow) -> Result<WorkflowView, String> {
    let (created_at, updated_at): (String, String) = db
        .query_row(
            "SELECT created_at, updated_at FROM workflows WHERE id = ?1",
            params![wf.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    Ok(WorkflowView {
        id: wf.id,
        name: wf.name.clone(),
        template: wf.template.clone(),
        repo: wf.repo.clone(),
        issue_number: wf.issue_number,
        issue_title: wf.issue_title.clone(),
        instructions: wf.instructions.clone(),
        pr_number: wf.pr_number,
        status: wf.status.clone(),
        detail: wf.detail.clone(),
        created_at,
        updated_at,
        steps: steps_of(db, wf.id)?.iter().map(StepRow::view).collect(),
    })
}

fn load_view(db: &Connection, id: i64) -> Result<WorkflowView, String> {
    view(db, &load_wf(db, id)?)
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// 记一条工作流事件，链回工作流页
fn record(db: &Connection, wf: &WfRow, kind: &str, level: &str, message: &str) {
    events::record(
        db,
        kind,
        level,
        &wf.name,
        message,
        Some(&format!("#/workflows?w={}", wf.id)),
    );
}

fn set_wf_status(
    db: &Connection,
    id: i64,
    status: &str,
    detail: Option<&str>,
) -> Result<(), String> {
    if !STATUSES.contains(&status) {
        return Err(format!("非法状态: {status}"));
    }
    db.execute(
        "UPDATE workflows SET status = ?1, detail = ?2, updated_at = datetime('now') WHERE id = ?3",
        params![status, detail, id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 依赖是否满足：硬依赖必须 done；软依赖 done 或 failed 都行
fn dep_satisfied(dep: &str, steps: &[StepRow]) -> bool {
    let (soft, seq_text) = match dep.strip_prefix('?') {
        Some(rest) => (true, rest),
        None => (false, dep),
    };
    let Ok(seq) = seq_text.parse::<i64>() else {
        return false;
    };
    steps
        .iter()
        .find(|s| s.seq == seq)
        .is_some_and(|s| match s.status.as_str() {
            "done" => true,
            "failed" | "skipped" => soft,
            _ => false,
        })
}

/// 硬依赖里有没有已经失败 / 被跳过的（有的话这一步永远跑不了）
fn dep_broken(dep: &str, steps: &[StepRow]) -> bool {
    let seq_text = dep.strip_prefix('?').unwrap_or(dep);
    let Ok(seq) = seq_text.parse::<i64>() else {
        return false;
    };
    steps
        .iter()
        .find(|s| s.seq == seq)
        .is_some_and(|s| s.status == "failed" || s.status == "skipped")
        && !dep_satisfied(dep, steps)
}

/// 一个步骤依赖的任务（评审门的候选、session 步骤的 diff 来源）
fn dep_task_ids(step: &StepRow, steps: &[StepRow]) -> Vec<(i64, i64, String)> {
    // (seq, task_id, 步骤名)
    steps
        .iter()
        .filter(|s| {
            s.kind == "task"
                && s.task_id.is_some()
                && step
                    .depends_on
                    .iter()
                    .any(|d| d.trim_start_matches('?') == s.seq.to_string())
        })
        .map(|s| (s.seq, s.task_id.unwrap(), s.name.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// 引擎
// ---------------------------------------------------------------------------

/// 推一把所有进行中的工作流。命令执行完与后台轮询都会调它
pub async fn tick(state: &Arc<AppState>) {
    let ids: Vec<i64> = {
        let db = state.db.lock().unwrap();
        let mut stmt = match db
            .prepare("SELECT id FROM workflows WHERE status IN ('running', 'waiting') ORDER BY id")
        {
            Ok(stmt) => stmt,
            Err(e) => {
                eprintln!("[workflow] {e}");
                return;
            }
        };
        stmt.query_map([], |row| row.get(0))
            .map_err(|e| e.to_string())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .collect()
    };
    for id in ids {
        if let Err(e) = advance(state, id).await {
            eprintln!("[workflow] 工作流 #{id} 推进失败：{e}");
        }
    }
}

async fn advance(state: &Arc<AppState>, wf_id: i64) -> Result<(), String> {
    let (wf, steps) = {
        let db = state.db.lock().unwrap();
        let wf = load_wf(&db, wf_id)?;
        let steps = steps_of(&db, wf_id)?;
        (wf, steps)
    };
    if !matches!(wf.status.as_str(), "running" | "waiting") {
        return Ok(());
    }

    // 1) 推进正在跑的步骤
    for step in steps.iter().filter(|s| s.status == "running") {
        advance_running(state, &wf, step).await;
    }

    // 2) 启动依赖已满足的步骤；一有步骤动过就重新看一遍（跳过的会级联）
    loop {
        let steps = {
            let db = state.db.lock().unwrap();
            steps_of(&db, wf_id)?
        };
        let mut changed = false;
        for step in steps.iter().filter(|s| s.status == "pending") {
            if step.depends_on.iter().any(|d| dep_broken(d, &steps)) {
                let db = state.db.lock().unwrap();
                db.execute(
                    "UPDATE workflow_steps SET status = 'skipped', detail = '上游失败，跳过', finished_at = datetime('now') WHERE id = ?1",
                    params![step.id],
                )
                .map_err(|e| e.to_string())?;
                record(
                    &db,
                    &wf,
                    "workflow.step_skipped",
                    "warning",
                    &format!("「{}」的上游失败，跳过", step.name),
                );
                changed = true;
                continue;
            }
            if step.depends_on.iter().all(|d| dep_satisfied(d, &steps)) {
                start_step(state, &wf, step).await;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // 3) 重算工作流的状态
    recompute(state, wf_id).await
}

/// 正在跑的步骤：看它挂着的任务 / 会话走到了哪
async fn advance_running(state: &Arc<AppState>, wf: &WfRow, step: &StepRow) {
    match step.kind.as_str() {
        "task" => {
            let Some(task_id) = step.task_id else {
                mark_step_failed(state, wf, step, "步骤挂着任务，但任务号丢了").await;
                return;
            };
            // 先把结果绑出来：match 的 scrutinee 里创建的锁会活到整个 match 结束，
            // Err 分支里的 await 就会带着它（future 会因此不是 Send）
            let loaded = tasks::load(&state.db.lock().unwrap(), task_id);
            let task = match loaded {
                Ok(task) => task,
                Err(e) => {
                    mark_step_failed(state, wf, step, &format!("任务读不出来了：{e}")).await;
                    return;
                }
            };
            match task.status.as_str() {
                // 改动就绪 / PR 已开 / 已合并，都算这一步做完了；往外发仍要人确认
                "changes" | "reviewing" | "done" => {
                    let outcome = json!({
                        "taskId": task.id,
                        "branch": task.branch,
                        "prUrl": task.pr_url,
                        "taskStatus": task.status,
                    })
                    .to_string();
                    let db = state.db.lock().unwrap();
                    db.execute(
                        "UPDATE workflow_steps SET status = 'done', outcome = ?1, detail = ?2, finished_at = datetime('now') WHERE id = ?3",
                        params![outcome, task.detail, step.id],
                    )
                    .map_err(|e| e.to_string())
                    .unwrap_or(0);
                    record(
                        &db,
                        wf,
                        "workflow.step_done",
                        "info",
                        &format!(
                            "「{}」完成（{}）",
                            step.name,
                            task.detail.as_deref().unwrap_or("改动就绪")
                        ),
                    );
                }
                "failed" => {
                    mark_step_failed(
                        state,
                        wf,
                        step,
                        task.failure.as_deref().unwrap_or("任务失败"),
                    )
                    .await;
                }
                _ => {}
            }
        }
        "session" => {
            let Some(session_id) = step.session_id else {
                mark_step_failed(state, wf, step, "步骤挂着会话，但会话号丢了").await;
                return;
            };
            if state.live.lock().unwrap().contains_key(&session_id) {
                return;
            }
            let last: Option<(String, String)> = state
                .db
                .lock()
                .unwrap()
                .query_row(
                    "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                    params![session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .ok()
                .flatten();
            match last {
                Some((role, content)) if role == "assistant" => {
                    let db = state.db.lock().unwrap();
                    db.execute(
                        "UPDATE workflow_steps SET status = 'done', outcome = ?1, finished_at = datetime('now') WHERE id = ?2",
                        params![clip(&content, OUTCOME_LIMIT), step.id],
                    )
                    .map_err(|e| e.to_string())
                    .unwrap_or(0);
                    record(
                        &db,
                        wf,
                        "workflow.step_done",
                        "info",
                        &format!("「{}」完成", step.name),
                    );
                }
                Some((role, content)) => {
                    let reason = if role == "system" {
                        format!("这一轮没有完成：{}", clip(&content, 400))
                    } else {
                        "会话没有产出回答（被停止或中断了）".into()
                    };
                    mark_step_failed(state, wf, step, &reason).await;
                }
                None => {
                    mark_step_failed(state, wf, step, "会话里没有任何消息").await;
                }
            }
        }
        _ => {}
    }
}

async fn mark_step_failed(state: &Arc<AppState>, wf: &WfRow, step: &StepRow, reason: &str) {
    let db = state.db.lock().unwrap();
    let _ = db.execute(
        "UPDATE workflow_steps SET status = 'failed', failure = ?1, finished_at = datetime('now') WHERE id = ?2",
        params![reason, step.id],
    );
    record(
        &db,
        wf,
        "workflow.step_failed",
        "warning",
        &format!("「{}」失败：{}", step.name, clip(reason, 160)),
    );
}

/// 启动一个依赖已满足的步骤。启动失败算这一步失败（不拖垮整个 tick）
async fn start_step(state: &Arc<AppState>, wf: &WfRow, step: &StepRow) {
    let result = match step.kind.as_str() {
        "task" => start_task_step(state, wf, step).await,
        "session" => start_session_step(state, wf, step).await,
        "gate" => Ok(()),
        other => Err(format!("不知道怎么跑的步骤类型：{other}")),
    };
    let db = state.db.lock().unwrap();
    match result {
        Ok(()) => {
            let (status, detail) = if step.kind == "gate" {
                ("waiting", "等你在候选方案里选一个".to_string())
            } else {
                ("running", String::new())
            };
            db.execute(
                "UPDATE workflow_steps SET status = ?1, detail = ?2, started_at = datetime('now') WHERE id = ?3",
                params![status, if detail.is_empty() { None } else { Some(detail.as_str()) }, step.id],
            )
            .map_err(|e| e.to_string())
            .unwrap_or(0);
            record(
                &db,
                wf,
                "workflow.step_started",
                "info",
                &format!("「{}」开始", step.name),
            );
            if step.kind == "gate" {
                record(
                    &db,
                    wf,
                    "workflow.gate",
                    "warning",
                    &format!("「{}」等你决策", step.name),
                );
            }
        }
        Err(e) => {
            let _ = db.execute(
                "UPDATE workflow_steps SET status = 'failed', failure = ?1, started_at = datetime('now'), finished_at = datetime('now') WHERE id = ?2",
                params![e, step.id],
            );
            record(
                &db,
                wf,
                "workflow.step_failed",
                "warning",
                &format!("「{}」启动失败：{}", step.name, clip(&e, 160)),
            );
        }
    }
}

/// 任务步骤：建任务（工作流专用的分支，免得并行的步骤撞在同一个分支上）再启动流水线
async fn start_task_step(state: &Arc<AppState>, wf: &WfRow, step: &StepRow) -> Result<(), String> {
    let adapter = step.conf_str("adapter");
    if adapter.trim().is_empty() {
        return Err("没有指定用哪个 Agent".into());
    }
    let workdir = step.conf_str("workdir");
    if workdir.trim().is_empty() {
        return Err("没有指定仓库在那台机器上的位置".into());
    }
    let task = {
        let db = state.db.lock().unwrap();
        let task = match (wf.issue_number, wf.instructions.as_deref()) {
            (Some(number), _) => tasks::create_issue(
                &db,
                &format!("{} · {}", wf.name, step.name),
                &wf.repo,
                number,
                &format!("https://github.com/{}/issues/{number}", wf.repo),
            )?,
            (_, Some(instructions)) if !instructions.trim().is_empty() => tasks::create_manual(
                &db,
                &format!("{} · {}", wf.name, step.name),
                &wf.repo,
                instructions,
            )?,
            _ => return Err("既没有 Issue 也没有需求说明，建不了任务".into()),
        };
        // 并行步骤各自一条分支：`agent/issue-7-w1`、`agent/issue-7-w2`
        let branch = format!(
            "{}-w{}",
            task.branch.as_deref().unwrap_or("agent/wf"),
            step.seq
        );
        db.execute(
            "UPDATE tasks SET branch = ?1, workflow_id = ?2 WHERE id = ?3",
            params![branch, wf.id, task.id],
        )
        .map_err(|e| e.to_string())?;
        db.execute(
            "UPDATE workflow_steps SET task_id = ?1 WHERE id = ?2",
            params![task.id, step.id],
        )
        .map_err(|e| e.to_string())?;
        tasks::load(&db, task.id)?
    };
    let opts = StartOptions {
        node_id: step.conf_str("node_id"),
        adapter,
        model: step.config["model"].as_str().map(Into::into),
        workdir,
        base_branch: None,
        auto_pr: step.config["auto_pr"].as_bool().unwrap_or(false),
    };
    pipeline::start(state, task.id, &opts).await.map(|_| ())
}

/// 会话步骤：把依赖的产物（diff、审查结论）拼进提示词，开一段会话
async fn start_session_step(
    state: &Arc<AppState>,
    wf: &WfRow,
    step: &StepRow,
) -> Result<(), String> {
    let adapter = step.conf_str("adapter");
    if adapter.trim().is_empty() {
        return Err("没有指定用哪个 Agent".into());
    }
    let prompt = build_session_prompt(state, wf, step).await?;
    let workdir = {
        let dir = step.conf_str("workdir");
        if dir.trim().is_empty() {
            None
        } else {
            Some(dir)
        }
    };
    let session = chat::start(
        state,
        Some(&step.conf_str("node_id")),
        &adapter,
        step.config["model"].as_str().map(Into::into),
        workdir.as_deref(),
        &prompt,
    )
    .await?;
    let db = state.db.lock().unwrap();
    db.execute(
        "UPDATE workflow_steps SET session_id = ?1 WHERE id = ?2",
        params![session, step.id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 拼 session 步骤的提示词：模板里的占位符换成真实的上下文
async fn build_session_prompt(
    state: &Arc<AppState>,
    wf: &WfRow,
    step: &StepRow,
) -> Result<String, String> {
    let mut prompt = step.config["prompt"]
        .as_str()
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| "这一步没有提示词".to_string())?
        .to_string();

    let task_desc = match (
        wf.issue_number,
        wf.issue_title.as_deref(),
        wf.instructions.as_deref(),
    ) {
        (Some(number), Some(title), _) => format!("Issue #{number}：{title}"),
        (Some(number), _, _) => format!("Issue #{number}"),
        (_, _, Some(instructions)) if !instructions.trim().is_empty() => {
            format!("需求：{}", clip(instructions.trim(), 800))
        }
        _ => "（未提供背景）".into(),
    };
    let pr = wf.pr_number.map(|n| n.to_string()).unwrap_or_default();

    let steps = {
        let db = state.db.lock().unwrap();
        steps_of(&db, wf.id)?
    };

    // {diff}：PR 审查取 PR 的 diff；其它取依赖任务工作树里的改动
    let diff = match step.conf_str("diff_source").as_str() {
        "pr" => {
            let Some(number) = wf.pr_number else {
                return Err("这个工作流没有关联 PR".into());
            };
            let token = github::token_of(state)?;
            let (owner, name) = wf
                .repo
                .split_once('/')
                .ok_or("仓库名要是 owner/repo 的样子")?;
            match github::pr_diff(state, &token, owner, name, number).await {
                Ok(text) if text.trim().is_empty() => "（这个 PR 没有文件改动）".into(),
                Ok(text) => clip(&text, DIFF_LIMIT),
                Err(e) => return Err(format!("取 PR 的 diff 失败：{e}")),
            }
        }
        "worktree" => {
            // 工作流里所有任务步骤的改动（写变更说明这类步骤不直接依赖任务步骤，但说明要基于改动）
            let mut out = String::new();
            for step_task in steps
                .iter()
                .filter(|s| s.kind == "task" && s.task_id.is_some())
            {
                let Some(task_id) = step_task.task_id else {
                    continue;
                };
                let Ok(task) = tasks::load(&state.db.lock().unwrap(), task_id) else {
                    continue;
                };
                let text = worktree_diff(state, &task).await;
                out.push_str(&format!(
                    "## 来自「{}」的改动\n\n{text}\n\n",
                    step_task.name
                ));
                if out.chars().count() > DIFF_LIMIT {
                    break;
                }
            }
            if out.is_empty() {
                return Err("这一步的提示词需要上游的改动，但工作流里还没有任务步骤".into());
            }
            clip(&out, DIFF_LIMIT)
        }
        _ => String::new(),
    };

    // {reviews}：依赖的会话步骤的结论
    let mut reviews = String::new();
    for dep in steps.iter().filter(|s| {
        s.kind == "session"
            && step
                .depends_on
                .iter()
                .any(|d| d.trim_start_matches('?') == s.seq.to_string())
    }) {
        let text = dep.outcome.as_deref().unwrap_or("（没有产出）");
        reviews.push_str(&format!(
            "### {} 的意见\n\n{}\n\n",
            dep.name,
            clip(text, REVIEW_LIMIT)
        ));
    }

    for (placeholder, value) in [
        ("{repo}", wf.repo.clone()),
        ("{task}", task_desc),
        ("{pr}", pr),
        ("{diff}", diff),
        ("{reviews}", reviews),
    ] {
        prompt = prompt.replace(placeholder, &value);
    }
    Ok(prompt)
}

/// 一个任务工作树里的全部改动（逐文件拼成 patch）
async fn worktree_diff(state: &Arc<AppState>, task: &Task) -> String {
    let diff = match pipeline::diff(state, task).await {
        Ok(diff) => diff,
        Err(e) => return format!("（读改动失败：{e}）"),
    };
    if diff.entries.is_empty() {
        return "（工作树里没有改动）".into();
    }
    let mut out = String::new();
    for entry in &diff.entries {
        let patch = pipeline::diff_file(state, task, &entry.path, entry.status == "?")
            .await
            .unwrap_or_else(|_| "（读不了这个文件的改动）".into());
        out.push_str(&format!(
            "--- {}\n{}\n\n",
            entry.path,
            clip(&patch, FILE_DIFF_LIMIT)
        ));
        if out.chars().count() > DIFF_LIMIT {
            break;
        }
    }
    clip(&out, DIFF_LIMIT)
}

/// 按步骤的完成情况重算工作流的状态
async fn recompute(state: &Arc<AppState>, wf_id: i64) -> Result<(), String> {
    let wf = {
        let db = state.db.lock().unwrap();
        load_wf(&db, wf_id)?
    };
    if !matches!(wf.status.as_str(), "running" | "waiting") {
        return Ok(());
    }
    let steps = {
        let db = state.db.lock().unwrap();
        steps_of(&db, wf_id)?
    };
    let done = steps.iter().filter(|s| s.status == "done").count();
    let all_terminal = steps
        .iter()
        .all(|s| matches!(s.status.as_str(), "done" | "failed" | "skipped"));
    let any_done = done > 0;
    let any_waiting = steps.iter().any(|s| s.status == "waiting");
    let mut status = "running";
    let mut detail = Some(format!("进行中：{done}/{} 步完成", steps.len()));
    let mut event: Option<(&str, &str, String)> = None;
    if !all_terminal && any_waiting {
        // 人工门开着：先等人，其余稍后再说
        status = "waiting";
        detail = Some("等你在候选方案里选一个".into());
        event = Some(("workflow.gate", "warning", "等你处理人工步骤".into()));
    } else if all_terminal && any_done {
        // 都有了结局（软依赖的分支失败也算走完），只要有一路成了就是完成
        status = "done";
        detail = if steps.iter().any(|s| s.status != "done") {
            Some("完成（有步骤失败或被跳过，详情在下面）".into())
        } else {
            None
        };
        event = Some(("workflow.done", "info", "全部步骤完成".into()));
    } else if all_terminal {
        let reason = steps
            .iter()
            .find(|s| s.status == "failed")
            .and_then(|s| s.failure.clone())
            .unwrap_or_else(|| "没有成功的步骤".into());
        status = "failed";
        detail = Some(clip(&reason, 300));
        event = Some(("workflow.failed", "warning", "所有步骤都没能完成".into()));
    } else if let Some(step) = steps.iter().find(|s| s.status == "failed") {
        // 还有步骤没走完，但已经有失败：等收尾，状态先标成失败（软依赖会把能跑的继续跑完）
        status = "failed";
        detail = step.failure.clone();
        event = Some((
            "workflow.failed",
            "warning",
            format!("「{}」失败", step.name),
        ));
    }
    let db = state.db.lock().unwrap();
    set_wf_status(&db, wf_id, status, detail.as_deref())?;
    if let Some((kind, level, message)) = event {
        record(&db, &wf, kind, level, &message);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

/// 建一个工作流（不启动）。Issue 的标题与地址界面上有；没传就拉一次
#[allow(clippy::too_many_arguments)]
pub async fn create(
    state: &Arc<AppState>,
    template: &str,
    repo: &str,
    issue_number: Option<i64>,
    issue_title: Option<String>,
    issue_url: Option<String>,
    pr_number: Option<i64>,
    instructions: Option<String>,
    node_id: &str,
    workdir: &str,
    model: Option<String>,
    auto_pr: bool,
    adapters: Vec<String>,
    models: Vec<String>,
) -> Result<WorkflowView, String> {
    let tpl = template_of(template)?;
    if tpl.live.is_some() {
        return Err("这个流程已经直接做在别的页面里，不用建工作流".into());
    }
    let (owner, repo_name) = pipeline::split_repo(repo)?;
    let instructions = instructions
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if tpl.needs_pr && pr_number.is_none_or(|n| n <= 0) {
        return Err("这个模板要处理一个 PR，请填 PR 号".into());
    }
    if workdir.trim().is_empty() {
        return Err("请给出仓库在机器上的位置".into());
    }
    if pr_number.is_none() && issue_number.is_none() && instructions.is_none() {
        return Err("请给出要处理的 Issue 号，或写一段需求说明".into());
    }

    // Issue 的标题与地址：界面上没有就去拉一次
    let (issue_title, issue_url) = if let Some(number) = issue_number {
        match (
            issue_title.filter(|t| !t.trim().is_empty()),
            issue_url.filter(|u| !u.trim().is_empty()),
        ) {
            (Some(title), Some(url)) => (Some(title), Some(url)),
            (title, url) => {
                let token = github::token_of(state)?;
                let issue = github::issue(state, &token, &owner, &repo_name, number).await?;
                (title.or(Some(issue.title)), url.or(Some(issue.html_url)))
            }
        }
    } else {
        (None, None)
    };

    let node = nodes::normalize(Some(node_id));
    let name = format!("{} · {}", tpl.name, repo);
    let wf_id = {
        let db = state.db.lock().unwrap();
        db.execute(
            "INSERT INTO workflows (name, template, repo, issue_number, issue_url, issue_title, pr_number, instructions, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending')",
            params![name, template, repo.trim(), issue_number, issue_url, issue_title, pr_number, instructions],
        )
        .map_err(|e| e.to_string())?;
        let wf_id = db.last_insert_rowid();
        for (i, ts) in tpl.steps.iter().enumerate() {
            let adapter = adapters
                .get(i)
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .or_else(|| ts.adapter.map(Into::into))
                .unwrap_or_default();
            // 每一步可以单独指定模型；没填的沿用整个流的
            let model = models
                .get(i)
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .or_else(|| model.clone());
            let config = match ts.kind {
                "task" => json!({
                    "adapter": adapter, "node_id": node, "workdir": workdir.trim(),
                    "model": model, "auto_pr": auto_pr,
                }),
                "session" => json!({
                    "adapter": adapter, "node_id": node, "workdir": workdir.trim(),
                    "model": model, "prompt": ts.prompt.unwrap_or_default(), "diff_source": ts.diff_source,
                }),
                _ => json!({}),
            };
            db.execute(
                "INSERT INTO workflow_steps (workflow_id, seq, name, kind, depends_on, config)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    wf_id,
                    (i + 1) as i64,
                    ts.name,
                    ts.kind,
                    serde_json::to_string(
                        &ts.deps.iter().map(|d| d.to_string()).collect::<Vec<_>>()
                    )
                    .unwrap(),
                    config.to_string(),
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        let wf = load_wf(&db, wf_id)?;
        record(
            &db,
            &wf,
            "workflow.created",
            "info",
            "已创建工作流（还没启动）",
        );
        wf_id
    };
    load_view(&state.db.lock().unwrap(), wf_id)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn workflow_create(
    state: State<'_, Arc<AppState>>,
    template: String,
    repo: String,
    issue_number: Option<i64>,
    issue_title: Option<String>,
    issue_url: Option<String>,
    pr_number: Option<i64>,
    instructions: Option<String>,
    node_id: String,
    workdir: String,
    model: Option<String>,
    auto_pr: Option<bool>,
    adapters: Option<Vec<String>>,
    models: Option<Vec<String>>,
) -> Result<WorkflowView, String> {
    create(
        state.inner(),
        &template,
        &repo,
        issue_number,
        issue_title,
        issue_url,
        pr_number,
        instructions,
        &node_id,
        &workdir,
        model,
        auto_pr.unwrap_or(false),
        adapters.unwrap_or_default(),
        models.unwrap_or_default(),
    )
    .await
}

/// 启动：立刻推一把，无依赖的步骤马上开工
pub async fn start(state: &Arc<AppState>, id: i64) -> Result<WorkflowView, String> {
    {
        let db = state.db.lock().unwrap();
        let wf = load_wf(&db, id)?;
        if wf.status != "pending" {
            return Err(match wf.status.as_str() {
                "failed" => "失败的工作流用「重跑」继续".to_string(),
                "done" => "这个工作流已经完成了".into(),
                "cancelled" => "已取消的工作流不能重新启动；重新建一个吧".into(),
                _ => "这个工作流已经在跑了".into(),
            });
        }
        set_wf_status(&db, id, "running", Some("刚启动"))?;
        record(&db, &wf, "workflow.started", "info", "工作流启动");
    }
    advance(state, id).await?;
    load_view(&state.db.lock().unwrap(), id)
}

#[tauri::command]
pub async fn workflow_start(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<WorkflowView, String> {
    start(state.inner(), id).await
}

/// 取消：正在跑的任务与会话都停掉，没跑的步骤标记跳过
pub async fn cancel(state: &Arc<AppState>, id: i64) -> Result<WorkflowView, String> {
    let wf = {
        let db = state.db.lock().unwrap();
        let wf = load_wf(&db, id)?;
        if !matches!(wf.status.as_str(), "pending" | "running" | "waiting") {
            return Err("这个工作流已经结束，不用取消".into());
        }
        wf
    };
    let steps = {
        let db = state.db.lock().unwrap();
        steps_of(&db, id)?
    };
    for step in steps.iter().filter(|s| s.status == "running") {
        match (step.kind.as_str(), step.task_id, step.session_id) {
            ("task", Some(task_id), _) => {
                let _ = pipeline::abort(state, task_id).await;
            }
            (_, _, Some(session_id)) => {
                let _ = chat::stop(state, session_id).await;
            }
            _ => {}
        }
    }
    let db = state.db.lock().unwrap();
    for step in steps
        .iter()
        .filter(|s| matches!(s.status.as_str(), "pending" | "running" | "waiting"))
    {
        let _ = db.execute(
            "UPDATE workflow_steps SET status = 'skipped', detail = '工作流已取消', finished_at = datetime('now') WHERE id = ?1",
            params![step.id],
        );
    }
    set_wf_status(&db, id, "cancelled", Some("已取消"))?;
    record(&db, &wf, "workflow.cancelled", "warning", "工作流已取消");
    load_view(&db, id)
}

#[tauri::command]
pub async fn workflow_cancel(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<WorkflowView, String> {
    cancel(state.inner(), id).await
}

/// 人工评审：在候选方案里选定一个（它的任务不动，其余保持原样）
pub async fn gate(
    state: &Arc<AppState>,
    workflow_id: i64,
    step_id: i64,
    chosen_task_id: i64,
) -> Result<WorkflowView, String> {
    let (wf, steps) = {
        let db = state.db.lock().unwrap();
        let wf = load_wf(&db, workflow_id)?;
        let steps = steps_of(&db, workflow_id)?;
        (wf, steps)
    };
    let step = steps
        .iter()
        .find(|s| s.id == step_id)
        .ok_or("这个步骤不属于这个工作流")?;
    if step.kind != "gate" {
        return Err("只有评审步骤需要人工决策".into());
    }
    if wf.status != "running" && wf.status != "waiting" {
        return Err("这个工作流没有在等你决策".into());
    }
    if step.status != "waiting" {
        return Err("这一步现在不用决策".into());
    }
    let candidates = dep_task_ids(step, &steps);
    let Some((_, _, chosen_title)) = candidates
        .iter()
        .find(|(_, task_id, _)| *task_id == chosen_task_id)
    else {
        return Err("选的任务不在这一步的候选里".into());
    };
    let chosen_title = chosen_title.clone();
    {
        let db = state.db.lock().unwrap();
        db.execute(
            "UPDATE workflow_steps SET status = 'done', outcome = ?1, detail = ?2, finished_at = datetime('now') WHERE id = ?3",
            params![
                json!({ "chosenTask": chosen_task_id, "chosenTitle": chosen_title }).to_string(),
                format!("选定「{chosen_title}」"),
                step_id,
            ],
        )
        .map_err(|e| e.to_string())?;
        record(
            &db,
            &wf,
            "workflow.step_done",
            "info",
            &format!("评审完成：选定「{chosen_title}」，其余方案保持原样"),
        );
    }
    advance(state, workflow_id).await?;
    load_view(&state.db.lock().unwrap(), workflow_id)
}

#[tauri::command]
pub async fn workflow_gate(
    state: State<'_, Arc<AppState>>,
    workflow_id: i64,
    step_id: i64,
    chosen_task_id: i64,
) -> Result<WorkflowView, String> {
    gate(state.inner(), workflow_id, step_id, chosen_task_id).await
}

/// 重跑失败的工作流：失败的、被跳过的步骤重新排队（任务步骤会建一个新任务，旧的失败任务留在任务页）
pub async fn retry(state: &Arc<AppState>, id: i64) -> Result<WorkflowView, String> {
    {
        let db = state.db.lock().unwrap();
        let wf = load_wf(&db, id)?;
        if wf.status != "failed" {
            return Err("只有失败的工作流能重跑".into());
        }
        for step in steps_of(&db, id)?
            .iter()
            .filter(|s| matches!(s.status.as_str(), "failed" | "skipped"))
        {
            db.execute(
                "UPDATE workflow_steps SET status = 'pending', task_id = NULL, session_id = NULL,
                        outcome = NULL, detail = NULL, failure = NULL, started_at = NULL, finished_at = NULL
                 WHERE id = ?1",
                params![step.id],
            )
            .map_err(|e| e.to_string())?;
        }
        set_wf_status(&db, id, "running", Some("重跑失败步骤"))?;
        record(&db, &wf, "workflow.started", "info", "重跑失败步骤");
    }
    advance(state, id).await?;
    load_view(&state.db.lock().unwrap(), id)
}

#[tauri::command]
pub async fn workflow_retry(
    state: State<'_, Arc<AppState>>,
    id: i64,
) -> Result<WorkflowView, String> {
    retry(state.inner(), id).await
}

#[tauri::command]
pub fn workflow_list(state: State<'_, Arc<AppState>>) -> Result<Vec<WorkflowView>, String> {
    let db = state.db.lock().unwrap();
    let ids: Vec<i64> = db
        .prepare_cached("SELECT id FROM workflows ORDER BY id DESC LIMIT 100")
        .map_err(|e| e.to_string())?
        .query_map([], |row| row.get(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    ids.iter().map(|id| load_view(&db, *id)).collect()
}

#[tauri::command]
pub fn workflow_get(state: State<'_, Arc<AppState>>, id: i64) -> Result<WorkflowView, String> {
    load_view(&state.db.lock().unwrap(), id)
}

/// 应用启动后调用：立刻做一次对账（重启时被中断的会话步骤会在这里判失败），然后定期推
pub fn start_polling(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(10));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            tick(&state).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use crate::testutil::{
        Collect, Req, TestState, fake_script, read_request, respond, setup_repo, test_state,
    };
    use autoagents_core::runner::BinaryResolver;
    use autoagents_core::service::{Service, ServiceOptions};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// 假的 GitHub API：工作流会调的接口（默认分支、Issue、PR 的 diff）
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
                    let Req { method, path, .. } = read_request(&mut stream).await;
                    calls
                        .lock()
                        .unwrap()
                        .push((method.clone(), path.clone(), String::new()));
                    macro_rules! json {
                        ($text:expr) => {
                            respond(&mut stream, "200 OK", "application/json", $text)
                        };
                    }
                    let route = path.split('?').next().unwrap_or("").to_string();
                    match (method.as_str(), route.as_str()) {
                        ("GET", "/repos/o/r") => json!(r#"{"default_branch": "main"}"#).await,
                        ("GET", "/repos/o/r/issues/7") => json!(
                            r#"{"number": 7, "title": "加一句问候", "body": "在 README 里加一句问候。", "html_url": "https://github.test/o/r/issues/7"}"#
                        )
                        .await,
                        // PR 详情：工作流取 diff 走的是纯文本接口
                        ("GET", "/repos/o/r/pulls/5") => respond(
                            &mut stream,
                            "200 OK",
                            "text/plain",
                            "diff --git a/app.txt b/app.txt\n--- a/app.txt\n+++ b/app.txt\n@@ -1 +1,2 @@\n hello\n+world\n",
                        )
                        .await,
                        _ => json!(r#"{"message": "Not Found"}"#).await,
                    }
                });
            }
        });
        fake
    }

    /// 写一个文件的「修 Issue」Agent（claude / grok 的 stream-json 协议）
    fn writer_agent(file: &str) -> String {
        format!(
            r#"echo hello > {file}
printf '%s\n' '{{"type":"result","is_error":false,"result":"新增了问候语。"}}'"#
        )
    }

    /// 只回一句话、不改文件的「审查」Agent（claude / grok 协议）
    fn talker_agent(reply: &str) -> String {
        format!(r#"printf '%s\n' '{{"type":"result","is_error":false,"result":"{reply}"}}'"#)
    }

    /// codex 是另一套协议：整条 agent_message + task_complete
    fn codex_writer_agent(file: &str) -> String {
        format!(
            r#"echo hello > {file}
printf '%s\n' '{{"type":"agent_message","message":"新增了问候语。"}}'
printf '%s\n' '{{"type":"task_complete"}}'"#
        )
    }

    fn codex_talker_agent(reply: &str) -> String {
        format!(
            r#"printf '%s\n' '{{"type":"agent_message","message":"{reply}"}}'
printf '%s\n' '{{"type":"task_complete"}}'"#
        )
    }

    /// 一直失败的 Agent
    const BROKEN_AGENT: &str = r#"echo 炸了 >&2
exit 7"#;

    /// 两个不同的 Agent（resolver 收到的是适配器 id，按它分发到各自的脚本）
    fn two_agent_service(pairs: Vec<(&str, &str)>) -> Arc<Service> {
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

    /// 反复推引擎，直到工作流落到想要的状态（或超时）
    async fn wait_wf(state: &Arc<AppState>, id: i64, want: &[&str]) -> WorkflowView {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
        loop {
            tick(state).await;
            let wf = {
                let db = state.db.lock().unwrap();
                load_view(&db, id).unwrap()
            };
            if want.contains(&wf.status.as_str()) {
                return wf;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "工作流一直停在 {}（{:?}）",
                wf.status,
                wf.steps
                    .iter()
                    .map(|s| (s.name.as_str(), s.status.as_str(), s.failure.as_deref()))
                    .collect::<Vec<_>>(),
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// 一个可以动手的工作目录 + 配好假 GitHub 的测试状态
    async fn rig(pairs: Vec<(&str, &str)>) -> (FakeGithub, TestState, std::path::PathBuf) {
        let github = start_github().await;
        let tmp = tempfile::tempdir().unwrap();
        let workdir = setup_repo(tmp.path());
        let ts = test_state(two_agent_service(pairs));
        *ts.state.gh_api.lock().unwrap() = github.addr.clone();
        ts.secrets.set("github_pat", "test-token").unwrap();
        std::mem::forget(tmp);
        (github, ts, workdir)
    }

    /// 建一个工作流（还没启动）
    async fn make(
        state: &Arc<AppState>,
        template: &str,
        repo: &str,
        issue: Option<i64>,
        pr: Option<i64>,
        instructions: Option<&str>,
        workdir: &std::path::Path,
        adapters: Vec<String>,
        models: Option<Vec<String>>,
    ) -> WorkflowView {
        create(
            state,
            template,
            repo,
            issue,
            None,
            None,
            pr,
            instructions.map(Into::into),
            "",
            &workdir.to_string_lossy(),
            None,
            false,
            adapters,
            models.unwrap_or_default(),
        )
        .await
        .unwrap()
    }

    /// 事件里某条消息的 id（按出现顺序断言用）
    fn event_id(state: &Arc<AppState>, needle: &str) -> Option<i64> {
        let db = state.db.lock().unwrap();
        db.query_row(
            "SELECT id FROM events WHERE message LIKE ?1 ORDER BY id LIMIT 1",
            params![format!("%{needle}%")],
            |row| row.get(0),
        )
        .ok()
    }

    /// 某个会话第一条用户消息（即发给 Agent 的提示词）
    fn prompt_of(state: &Arc<AppState>, session_id: i64) -> String {
        state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT content FROM messages WHERE session_id = ?1 AND role = 'user' ORDER BY id LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn race_runs_both_agents_in_parallel_then_the_gate_picks() {
        let (_gh, ts, workdir) = rig(vec![
            ("claude-cli", &writer_agent("greet_a.txt")),
            ("codex-cli", &codex_writer_agent("greet_b.txt")),
        ])
        .await;
        let wf = make(
            &ts.state,
            "race",
            "o/r",
            Some(7),
            None,
            None,
            &workdir,
            vec![],
            None,
        )
        .await;
        assert_eq!(wf.steps.len(), 3, "{wf:?}");
        let wf = start(&ts.state, wf.id).await.unwrap();
        assert_eq!(wf.status, "running");

        // 两个任务步骤并行跑，各自一条分支
        let wf = wait_wf(&ts.state, wf.id, &["waiting"]).await;
        let task_steps: Vec<_> = wf.steps.iter().filter(|s| s.kind == "task").collect();
        assert_eq!(task_steps.len(), 2);
        assert!(task_steps.iter().all(|s| s.status == "done"), "{wf:?}");
        let branches: Vec<String> = {
            let db = ts.state.db.lock().unwrap();
            db.prepare("SELECT branch FROM tasks ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(branches.len(), 2, "{branches:?}");
        assert!(branches.iter().any(|b| b.ends_with("-w1")), "{branches:?}");
        assert!(branches.iter().any(|b| b.ends_with("-w2")), "{branches:?}");
        // 两个工作树都真的有产出
        let worktrees: Vec<String> = {
            let db = ts.state.db.lock().unwrap();
            db.prepare("SELECT worktree FROM tasks WHERE worktree IS NOT NULL ORDER BY id")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(worktrees.len(), 2, "{worktrees:?}");
        assert!(worktrees[0].ends_with("agent-issue-7-w1"), "{worktrees:?}");
        assert!(worktrees[1].ends_with("agent-issue-7-w2"), "{worktrees:?}");
        assert!(
            std::path::Path::new(&worktrees[0])
                .join("greet_a.txt")
                .exists()
        );
        assert!(
            std::path::Path::new(&worktrees[1])
                .join("greet_b.txt")
                .exists()
        );

        // 人工评审：选定方案 A 之后整个工作流完成
        let gate_step = wf.steps.iter().find(|s| s.kind == "gate").unwrap();
        assert_eq!(gate_step.status, "waiting");
        let winner = task_steps[0].task_id.unwrap();
        let wf = gate(&ts.state, wf.id, gate_step.id, winner).await.unwrap();
        assert_eq!(wf.status, "done", "{wf:?}");
        let outcome: Value = serde_json::from_str(wf.steps[2].outcome.as_deref().unwrap()).unwrap();
        assert_eq!(outcome["chosenTask"], winner);
        // 选定与完成都进了活动流
        assert!(event_id(&ts.state, "评审完成：选定").is_some());
        assert!(event_id(&ts.state, "全部步骤完成").is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn issue_fix_chain_carries_context_between_steps() {
        let (_gh, ts, workdir) = rig(vec![
            ("claude-cli", &writer_agent("greet.txt")),
            (
                "codex-cli",
                &codex_talker_agent("问题：测试没加。总评：可以接受。"),
            ),
            ("grok-cli", &talker_agent("# 变更说明\\n\\n加了一句问候。")),
        ])
        .await;
        let wf = make(
            &ts.state,
            "issue-fix",
            "o/r",
            Some(7),
            None,
            None,
            &workdir,
            vec![],
            None,
        )
        .await;
        let wf = start(&ts.state, wf.id).await.unwrap();
        let wf = wait_wf(&ts.state, wf.id, &["done"]).await;
        assert!(wf.steps.iter().all(|s| s.status == "done"), "{wf:?}");

        // 只有第一步是任务步骤：一个任务、三个会话（任务会话 + 两个审查会话）
        let counts: (i64, i64) = {
            let db = ts.state.db.lock().unwrap();
            (
                db.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
                    .unwrap(),
                db.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
                    .unwrap(),
            )
        };
        assert_eq!(counts, (1, 3), "{counts:?}");

        // 审查一步拿到的提示词里有上游的 diff 与需求背景
        let review_prompt = prompt_of(&ts.state, wf.steps[1].session_id.unwrap());
        assert!(
            review_prompt.contains("Issue #7：加一句问候"),
            "{review_prompt}"
        );
        assert!(review_prompt.contains("+hello"), "{review_prompt}");
        // 写说明一步拿到审查的结论
        let notes_prompt = prompt_of(&ts.state, wf.steps[2].session_id.unwrap());
        assert!(notes_prompt.contains("测试没加"), "{notes_prompt}");
        assert!(notes_prompt.contains("+hello"), "{notes_prompt}");
        // 最后一步的产出就是变更说明
        assert!(
            wf.steps[2].outcome.as_deref().unwrap().contains("变更说明"),
            "{wf:?}"
        );

        // 顺序对：审查是在修复完成之后才开始的
        let fix_done = event_id(&ts.state, "「分析修复」完成").unwrap();
        let review_started = event_id(&ts.state, "「交叉审查」开始").unwrap();
        assert!(review_started > fix_done);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failure_cascades_and_retry_recreates_the_step() {
        let (_gh, ts, workdir) = rig(vec![
            ("claude-cli", BROKEN_AGENT),
            ("codex-cli", &talker_agent("没问题。")),
            ("grok-cli", &talker_agent("说明。")),
        ])
        .await;
        let wf = make(
            &ts.state,
            "issue-fix",
            "o/r",
            Some(7),
            None,
            None,
            &workdir,
            vec![],
            None,
        )
        .await;
        let wf = start(&ts.state, wf.id).await.unwrap();
        let wf = wait_wf(&ts.state, wf.id, &["failed"]).await;

        assert_eq!(wf.steps[0].status, "failed", "{wf:?}");
        assert!(
            wf.steps[0].failure.as_deref().unwrap().contains("完成"),
            "{}",
            wf.steps[0].failure.as_deref().unwrap_or("")
        );
        assert_eq!(wf.steps[1].status, "skipped");
        assert_eq!(wf.steps[1].detail.as_deref(), Some("上游失败，跳过"));
        assert_eq!(wf.steps[2].status, "skipped", "硬依赖断掉的步骤要级联跳过");
        let first_task = wf.steps[0].task_id.unwrap();

        // 重跑：失败的与跳过的重新排队；任务步骤建一个新任务
        let wf = retry(&ts.state, wf.id).await.unwrap();
        assert_eq!(wf.status, "running");
        let wf = wait_wf(&ts.state, wf.id, &["failed"]).await;
        assert_eq!(wf.steps[0].status, "failed");
        assert_ne!(
            wf.steps[0].task_id.unwrap(),
            first_task,
            "重跑要建新任务，不复活旧的"
        );
        assert_eq!(wf.steps[1].status, "skipped", "{wf:?}");
        let tasks: i64 = ts
            .state
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tasks, 2, "旧任务留在任务页，重跑建了新任务");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn soft_dependency_lets_the_gate_open_when_one_branch_fails() {
        let (_gh, ts, workdir) = rig(vec![
            ("claude-cli", &writer_agent("greet_a.txt")),
            ("codex-cli", BROKEN_AGENT),
        ])
        .await;
        let wf = make(
            &ts.state,
            "race",
            "o/r",
            Some(7),
            None,
            None,
            &workdir,
            vec![],
            None,
        )
        .await;
        let wf = start(&ts.state, wf.id).await.unwrap();
        // 方案 B 失败了，评审门照样打开（软依赖）
        let wf = wait_wf(&ts.state, wf.id, &["waiting"]).await;
        assert_eq!(wf.steps[0].status, "done", "{wf:?}");
        assert_eq!(wf.steps[1].status, "failed");
        assert_eq!(wf.steps[2].status, "waiting");
        let winner = wf.steps[0].task_id.unwrap();
        let gate_step = wf.steps.iter().find(|s| s.kind == "gate").unwrap();
        let wf = gate(&ts.state, wf.id, gate_step.id, winner).await.unwrap();
        assert_eq!(wf.status, "done", "{wf:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pr_review_feeds_the_pr_diff_to_both_agents() {
        let (_gh, ts, workdir) = rig(vec![
            ("claude-cli", &talker_agent("结论：需修改。")),
            ("codex-cli", &codex_talker_agent("结论：通过。")),
            ("grok-cli", &talker_agent("汇总：一处需修改，其余通过。")),
        ])
        .await;
        let wf = make(
            &ts.state,
            "pr-review",
            "o/r",
            None,
            Some(5),
            None,
            &workdir,
            // 汇总结论这步模板里不指定 Agent，由创建时选
            vec!["claude-cli".into(), "codex-cli".into(), "grok-cli".into()],
            None,
        )
        .await;
        let wf = start(&ts.state, wf.id).await.unwrap();
        let wf = wait_wf(&ts.state, wf.id, &["done"]).await;

        // 两个并行的审查步骤都拿到了 PR 的 diff
        for step in &wf.steps[..2] {
            assert_eq!(step.status, "done", "{wf:?}");
            let prompt = prompt_of(&ts.state, step.session_id.unwrap());
            assert!(prompt.contains("PR #5"), "{prompt}");
            assert!(prompt.contains("diff --git a/app.txt"), "{prompt}");
        }
        // 汇总一步拿到两份结论
        let sum_prompt = prompt_of(&ts.state, wf.steps[2].session_id.unwrap());
        assert!(sum_prompt.contains("需修改"), "{sum_prompt}");
        assert!(sum_prompt.contains("通过"), "{sum_prompt}");
        assert!(sum_prompt.contains("PR #5"), "{sum_prompt}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_stops_running_steps_and_marks_the_rest_skipped() {
        let (_gh, ts, workdir) = rig(vec![
            ("claude", &writer_agent("greet.txt")),
            ("codex", &talker_agent("没问题。")),
            ("grok", &talker_agent("说明。")),
        ])
        .await;
        let wf = make(
            &ts.state,
            "issue-fix",
            "o/r",
            Some(7),
            None,
            None,
            &workdir,
            vec![],
            None,
        )
        .await;
        let wf = start(&ts.state, wf.id).await.unwrap();

        // 等第一步的会话真的登记下来（chat::start 返回之后）再取消，
        // 免得取消时流水线还没来得及把会话挂上
        let mut running: Option<(i64, i64)> = None; // (task_id, session_id)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while running.is_none() {
            assert!(std::time::Instant::now() < deadline, "第一步一直没启动");
            let db = ts.state.db.lock().unwrap();
            running = db
                .query_row(
                    "SELECT s.task_id, t.session_id FROM workflow_steps s
                     JOIN tasks t ON t.id = s.task_id
                     WHERE s.workflow_id = ?1 AND s.seq = 1 AND t.session_id IS NOT NULL",
                    params![wf.id],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
                )
                .ok();
            drop(db);
            if running.is_none() {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }
        let (task_id, _) = running.unwrap();
        let wf = cancel(&ts.state, wf.id).await.unwrap();
        assert_eq!(wf.status, "cancelled", "{wf:?}");
        // 假 Agent 可能已经把第一步跑完：跑完的留着，没跑完的都标成跳过
        for step in &wf.steps {
            assert!(
                matches!(step.status.as_str(), "skipped" | "done"),
                "{:?}",
                (step.name.as_str(), step.status.as_str())
            );
        }
        // 停掉的任务留着；停止是异步收尾，等它落定（正常结束或失败，不该还在跑）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let task = tasks::load(&ts.state.db.lock().unwrap(), task_id).unwrap();
            if task.status != "running" {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "取消之后任务还停在 running"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "手动演示（真实 CLI + 真实模型）：cargo test -p autoagents --lib race_demo -- --ignored --nocapture。\
               第二阶段评审：AUTOAGENTS_RACE_DEMO=choose AUTOAGENTS_RACE_DEMO_CHOSEN=<任务号> 重跑同一条命令"]
    async fn race_demo() {
        let phase = std::env::var("AUTOAGENTS_RACE_DEMO").unwrap_or_else(|_| "run".into());
        let root = std::path::PathBuf::from("/tmp/aa-race-demo");
        std::fs::create_dir_all(&root).unwrap();

        // 假的 GitHub API：流水线只需要「默认分支是 main」这一条，一切都不出网
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gh = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let Req { method, path, .. } = read_request(&mut stream).await;
                    let route = path.split('?').next().unwrap_or("").to_string();
                    match (method.as_str(), route.as_str()) {
                        ("GET", "/repos/me/greet") => {
                            respond(
                                &mut stream,
                                "200 OK",
                                "application/json",
                                r#"{"default_branch": "main"}"#,
                            )
                            .await
                        }
                        _ => {
                            respond(
                                &mut stream,
                                "200 OK",
                                "application/json",
                                r#"{"login": "demo", "id": 1}"#,
                            )
                            .await
                        }
                    }
                });
            }
        });

        // 假远端 + 一份克隆（只建一次，重跑沿用）
        let remote = root.join("github.com/me/greet.git");
        let workdir = root.join("machine/greet");
        if !remote.exists() {
            std::fs::create_dir_all(&remote).unwrap();
            let git = |dir: &std::path::Path, args: &[&str]| {
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(["-c", "user.name=demo", "-c", "user.email=demo@demo"])
                    .args(args)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .output()
                    .unwrap()
            };
            assert!(
                git(&remote, &["init", "--bare", "-b", "main"])
                    .status
                    .success()
            );
            let seed = root.join("seed");
            std::fs::create_dir_all(&seed).unwrap();
            assert!(git(&seed, &["init", "-b", "main"]).status.success());
            std::fs::write(seed.join("README.md"), "# greet\n\n竞争模式的演示仓库。\n").unwrap();
            assert!(git(&seed, &["add", "-A"]).status.success());
            assert!(git(&seed, &["commit", "-m", "init"]).status.success());
            assert!(
                git(
                    &seed,
                    &["remote", "add", "origin", remote.to_str().unwrap()]
                )
                .status
                .success()
            );
            assert!(
                git(&seed, &["push", "-q", "origin", "main"])
                    .status
                    .success()
            );
            std::fs::create_dir_all(workdir.parent().unwrap()).unwrap();
            assert!(
                git(
                    &workdir.parent().unwrap(),
                    &["clone", "-q", remote.to_str().unwrap(), "greet"]
                )
                .status
                .success()
            );
        }

        // 包装脚本：给无头模式注入「允许写文件」的开关（只作用于这个演示，不改全局配置）
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let claude_bin = which("claude");
        let codex_bin = which("codex");
        std::fs::write(
            bin.join("claude"),
            format!(
                "#!/bin/sh\nexec {} --permission-mode acceptEdits \"$@\"\n",
                claude_bin
            ),
        )
        .unwrap();
        std::fs::write(
            bin.join("codex"),
            format!(
                "#!/bin/sh\nexec {} -c sandbox_mode='workspace-write' \"$@\"\n",
                codex_bin
            ),
        )
        .unwrap();
        for f in [bin.join("claude"), bin.join("codex")] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // 真实引擎 + 只认这两个包装脚本的 Service；用文件数据库，评审在另一次运行里做
        let map: HashMap<String, PathBuf> = HashMap::from([
            ("claude-cli".into(), bin.join("claude")),
            ("codex-cli".into(), bin.join("codex")),
        ]);
        let resolve: BinaryResolver = Arc::new(move |name: &str| map.get(name).cloned());
        let service = Service::new(ServiceOptions {
            name: Some("race-demo".into()),
            resolve: Some(resolve),
            ..Default::default()
        });
        let state = crate::AppState::new(
            crate::db::init(&root.join("state.db")).unwrap(),
            service,
            Arc::new(Collect::<crate::chat::ChatEvent>::default()),
            Arc::new(Collect::<crate::remote::NodeStatusEvent>::default()),
            Arc::new(crate::secrets::MemoryStore::default()),
        );
        *state.gh_api.lock().unwrap() = gh.clone();
        state.secrets.set("github_pat", "demo").unwrap();

        if phase == "choose" {
            let chosen: i64 = std::env::var("AUTOAGENTS_RACE_DEMO_CHOSEN")
                .expect("评审阶段要给 AUTOAGENTS_RACE_DEMO_CHOSEN=<任务号>")
                .parse()
                .expect("任务号要是数字");
            let db = state.db.lock().unwrap();
            let gate_row: Option<(i64, i64)> = db.query_row(
                "SELECT w.id, s.id FROM workflows w JOIN workflow_steps s ON s.workflow_id = w.id AND s.kind = 'gate'
                 WHERE w.status IN ('running', 'waiting') ORDER BY w.id DESC LIMIT 1",
                [], |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional().unwrap();
            let (wf_id, gate_id) = match gate_row {
                Some(pair) => pair,
                None => {
                    let wf_id: i64 = db
                        .query_row(
                            "SELECT id FROM workflows ORDER BY id DESC LIMIT 1",
                            [],
                            |r| r.get(0),
                        )
                        .optional()
                        .unwrap()
                        .expect("没有工作流可评审");
                    let wf = load_view(&db, wf_id).unwrap();
                    println!("工作流 #{} 已经是 {}，不用再评审", wf.id, wf.status);
                    return;
                }
            };
            drop(db);
            let wf = gate(&state, wf_id, gate_id, chosen).await.unwrap();
            println!("=== 评审完成 ===");
            println!("工作流 #{} 状态：{}", wf.id, wf.status);
            for s in &wf.steps {
                println!(
                    "  [{}] {} —— {}{}",
                    s.seq,
                    s.name,
                    s.status,
                    s.detail
                        .as_deref()
                        .map(|d| format!("（{d}）"))
                        .unwrap_or_default()
                );
            }
            return;
        }

        // 第一阶段：建流（只建一次）、启动、推进到评审门
        let existing: Option<i64> = {
            let db = state.db.lock().unwrap();
            db.query_row(
                "SELECT id FROM workflows ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
        };
        let wf = match existing {
            Some(id) => { let db = state.db.lock().unwrap(); load_view(&db, id).unwrap() }
            None => create(
                &state, "race", "me/greet", None, None, None, None,
                Some("给这个仓库写一个 greet.sh：打印一句问候语（文案和实现自选），加上可执行权限；再写一个 GREET-README.md 说明怎么用。不要提交。".into()),
                "", workdir.to_str().unwrap(), Some("glm-5.3-flash".into()), false, vec![],
                vec![],
            ).await.unwrap(),
        };
        if wf.status == "pending" {
            start(&state, wf.id).await.unwrap();
        }
        println!("=== 竞争模式开始 ===");
        println!(
            "仓库：{}（假远端 {}，不出网）",
            workdir.display(),
            remote.display()
        );
        println!("两个 Agent 并行开工（glm-5.3-flash），每 500ms 推一次引擎……");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        loop {
            tick(&state).await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let db = state.db.lock().unwrap();
            let wf = load_view(&db, wf.id).unwrap();
            if wf.status == "waiting" {
                println!("\n=== 评审门打开了 ===");
                for s in wf.steps.iter().filter(|s| s.kind == "task") {
                    let task = tasks::load(&db, s.task_id.unwrap()).unwrap();
                    let reply: Option<String> = task.session_id.and_then(|sid| {
                        db.query_row(
                            "SELECT content FROM messages WHERE session_id = ?1 AND role = 'assistant' ORDER BY id DESC LIMIT 1",
                            params![sid], |r| r.get::<_, String>(0),
                        ).optional().unwrap()
                    });
                    println!(
                        "\n[方案 {}] {} · 任务 #{} · 分支 {}",
                        s.name,
                        s.adapter.as_deref().unwrap_or("?"),
                        task.id,
                        task.branch.as_deref().unwrap_or("?")
                    );
                    println!("  工作树：{}", task.worktree.as_deref().unwrap_or("?"));
                    println!(
                        "  Agent 回复：{}",
                        reply
                            .as_deref()
                            .map(|t| clip(t, 300))
                            .unwrap_or_else(|| "（无）".into())
                    );
                    let wt = task.worktree.clone().unwrap_or_default();
                    let out = std::process::Command::new("git")
                        .arg("-C")
                        .arg(&wt)
                        .args(["status", "--short"])
                        .output()
                        .unwrap();
                    println!("  改动：{}", String::from_utf8_lossy(&out.stdout).trim());
                }
                println!(
                    "\n→ 你来当裁判：AUTOAGENTS_RACE_DEMO=choose AUTOAGENTS_RACE_DEMO_CHOSEN=<任务号> \\"
                );
                println!("  cargo test -p autoagents --lib race_demo -- --ignored --nocapture");
                return;
            }
            if wf.status == "failed" {
                println!(
                    "工作流失败了：{:?}",
                    wf.steps
                        .iter()
                        .map(|s| (s.name.as_str(), s.status.as_str(), s.failure.as_deref()))
                        .collect::<Vec<_>>()
                );
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "10 分钟还没跑到评审门"
            );
        }
    }

    /// PATH 上的真实可执行文件
    fn which(name: &str) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {name}"))
            .output()
            .expect("找可执行文件");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn create_validates_the_template_requirements() {
        let (_gh, ts, workdir) = rig(vec![("claude-cli", &writer_agent("greet.txt"))]).await;
        // 发布守护不用建工作流
        let e = create(
            &ts.state,
            "publish-guard",
            "o/r",
            None,
            None,
            None,
            None,
            None,
            "",
            workdir.to_str().unwrap(),
            None,
            false,
            vec![],
            vec![],
        )
        .await
        .unwrap_err();
        assert!(e.contains("不用建工作流"), "{e}");
        // PR 审查必须给 PR 号
        let e = create(
            &ts.state,
            "pr-review",
            "o/r",
            None,
            None,
            None,
            None,
            None,
            "",
            workdir.to_str().unwrap(),
            None,
            false,
            vec![],
            vec![],
        )
        .await
        .unwrap_err();
        assert!(e.contains("PR"), "{e}");
        // 既没有 Issue 也没有说明
        let e = create(
            &ts.state,
            "race",
            "o/r",
            None,
            None,
            None,
            None,
            None,
            "",
            workdir.to_str().unwrap(),
            None,
            false,
            vec![],
            vec![],
        )
        .await
        .unwrap_err();
        assert!(e.contains("Issue 号") || e.contains("需求说明"), "{e}");
        // 没给仓库位置
        let e = create(
            &ts.state,
            "race",
            "o/r",
            Some(7),
            None,
            None,
            None,
            None,
            "",
            "",
            None,
            false,
            vec![],
            vec![],
        )
        .await
        .unwrap_err();
        assert!(e.contains("位置"), "{e}");
        // 界面上带了 Issue 信息时不再去拉接口
        let wf = create(
            &ts.state,
            "issue-fix",
            "o/r",
            Some(7),
            Some("加一句问候".into()),
            Some("https://github.test/o/r/issues/7".into()),
            None,
            None,
            "",
            workdir.to_str().unwrap(),
            None,
            false,
            vec![],
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(wf.issue_title.as_deref(), Some("加一句问候"));
        assert_eq!(wf.steps.len(), 3);
        // 模板清单给界面的形状
        let templates = workflow_templates();
        assert_eq!(
            templates
                .iter()
                .find(|t| t.id == "race")
                .unwrap()
                .steps
                .len(),
            3
        );

        // 每一步可以单独指定模型：没填的沿用整个流的
        let workdir = std::env::temp_dir().join(format!("aa-wf-models-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let wf = create(
            &ts.state,
            "issue-fix",
            "o/r",
            Some(7),
            Some("加一句问候".into()),
            Some("https://github.test/o/r/issues/7".into()),
            None,
            None,
            "",
            workdir.to_str().unwrap(),
            Some("glm-5.3".into()),
            false,
            vec!["claude-cli".into(), "claude-cli".into(), "codex-cli".into()],
            vec!["opus".into(), String::new(), "gpt-5".into()],
        )
        .await
        .unwrap();
        let models: Vec<Option<String>> = wf.steps.iter().map(|s| s.model.clone()).collect();
        assert_eq!(
            models,
            vec![
                Some("opus".into()),
                Some("glm-5.3".into()),
                Some("gpt-5".into())
            ],
            "第一步用自己指定的，第二步空着沿用全局的，第三步用自己指定的"
        );
        let _ = std::fs::remove_dir_all(&workdir);
    }
}

//! 决策中心：把散在各页、等人拍板的事聚到一处。
//! 工作流的评审门、改动待确认的任务、还没开工的任务、PR 已开等验收——
//! 「我只负责决策和验收」里的决策全在这一个页面上做完，不用再一页页翻。

use crate::{AppState, github, pipeline, tasks};
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

/// 等人决策的评审门（竞争模式的择优）
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxGate {
    workflow_id: i64,
    workflow_name: String,
    step_id: i64,
    step_name: String,
    repo: String,
}

/// 等人处理的任务（改动待确认 / 还没开工）
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxTask {
    id: i64,
    title: String,
    repo: Option<String>,
    branch: Option<String>,
    detail: Option<String>,
    updated_at: String,
}

/// PR 已开、等人验收的任务
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxReview {
    id: i64,
    title: String,
    repo: Option<String>,
    pr_number: i64,
    pr_url: Option<String>,
    /// 检查的整体状态（没取到时为空）：success / pending / failure
    checks: Option<String>,
    detail: Option<String>,
    updated_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxView {
    gates: Vec<InboxGate>,
    changes: Vec<InboxTask>,
    starts: Vec<InboxTask>,
    reviews: Vec<InboxReview>,
}

fn inbox_task(task: &tasks::Task) -> InboxTask {
    InboxTask {
        id: task.id,
        title: task.title.clone(),
        repo: task.repo.clone(),
        branch: task.branch.clone(),
        detail: task.detail.clone(),
        updated_at: task.updated_at.clone(),
    }
}

/// 检查的整体结论：任一失败即 failure，否则有一项没完成就是 pending
fn checks_state(task: &tasks::Task) -> Option<String> {
    let checks = task.checks.as_ref()?.as_array()?;
    let states: Vec<&str> = checks
        .iter()
        .filter_map(|c| c.get("state").and_then(|s| s.as_str()))
        .collect();
    if states.is_empty() {
        return None;
    }
    let verdict = if states.contains(&"failure") {
        "failure"
    } else if states.contains(&"pending") {
        "pending"
    } else {
        "success"
    };
    Some(verdict.to_string())
}

#[tauri::command]
pub fn inbox_list(state: State<'_, Arc<AppState>>) -> Result<InboxView, String> {
    view(state.inner())
}

pub fn view(state: &Arc<AppState>) -> Result<InboxView, String> {
    let db = state.db.lock().unwrap();
    // 评审门：竞争模式停在人工择优
    let gates = db
        .prepare(
            "SELECT w.id, w.name, s.id, s.name, w.repo \
             FROM workflow_steps s JOIN workflows w ON w.id = s.workflow_id \
             WHERE s.kind = 'gate' AND s.status = 'waiting' ORDER BY s.id",
        )
        .map_err(|e| e.to_string())?
        .query_map([], |row| {
            Ok(InboxGate {
                workflow_id: row.get(0)?,
                workflow_name: row.get(1)?,
                step_id: row.get(2)?,
                step_name: row.get(3)?,
                repo: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect::<Vec<_>>();
    let changes = tasks::in_status(&db, "changes")?
        .iter()
        .map(inbox_task)
        .collect::<Vec<_>>();
    let starts = tasks::in_status(&db, "pending")?
        .iter()
        .map(inbox_task)
        .collect::<Vec<_>>();
    let reviews = tasks::in_status(&db, "reviewing")?
        .iter()
        .filter(|t| t.pr_number.is_some())
        .map(|task| InboxReview {
            id: task.id,
            title: task.title.clone(),
            repo: task.repo.clone(),
            pr_number: task.pr_number.unwrap_or_default(),
            pr_url: task.pr_url.clone(),
            checks: checks_state(task),
            detail: task.detail.clone(),
            updated_at: task.updated_at.clone(),
        })
        .collect::<Vec<_>>();
    Ok(InboxView {
        gates,
        changes,
        starts,
        reviews,
    })
}

/// 合并任务的 PR——验收的最后一步。合并后走一遍刷新，任务自动标完成
#[tauri::command]
pub async fn inbox_merge_pr(
    state: State<'_, Arc<AppState>>,
    task_id: i64,
) -> Result<tasks::Task, String> {
    merge_pr(state.inner(), task_id).await
}

pub async fn merge_pr(state: &Arc<AppState>, task_id: i64) -> Result<tasks::Task, String> {
    let (repo, number) = {
        let db = state.db.lock().unwrap();
        let task = tasks::load(&db, task_id)?;
        (task.repo, task.pr_number)
    };
    let Some(repo) = repo else {
        return Err("这个任务没有关联仓库".into());
    };
    let Some(number) = number else {
        return Err("这个任务还没有开 PR".into());
    };
    let (owner, repo_name) = pipeline::split_repo(&repo)?;
    let token = github::token_of(state)?;
    github::merge_pr(state, &token, &owner, &repo_name, number).await?;
    pipeline::refresh(state, task_id).await
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::secrets::SecretStore;
    use crate::testutil::{Req, TestState, read_request, respond, test_state};
    use autoagents_core::service::{Service, ServiceOptions};

    /// 假的 GitHub：只处理 PR #3 的合并与详情
    async fn fake_github() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let Req { method, path, .. } = read_request(&mut stream).await;
                    let route = path.split('?').next().unwrap_or("").to_string();
                    match (method.as_str(), route.as_str()) {
                        ("PUT", "/repos/o/r/pulls/3/merge") => respond(
                            &mut stream, "200 OK", "application/json",
                            r#"{"merged": true, "message": "ok"}"#,
                        )
                        .await,
                        // 合并后刷新任务：PR 已合并，任务该标完成
                        ("GET", "/repos/o/r/pulls/3") => respond(
                            &mut stream, "200 OK", "application/json",
                            r#"{"number": 3, "state": "closed", "merged": true, "merged_at": "2026-11-25T00:00:00Z", "head": {"sha": "abc123", "ref": "agent/task-3"}, "html_url": "https://github.test/o/r/pull/3"}"#,
                        )
                        .await,
                        _ => respond(&mut stream, "200 OK", "application/json", r#"{"message": "Not Found"}"#).await,
                    }
                });
            }
        });
        addr
    }

    fn seed(ts: &TestState) {
        let db = ts.state.db.lock().unwrap();
        // 一个等待决策的评审门
        db.execute(
            "INSERT INTO workflows (name, template, repo, status) VALUES ('竞争模式 · o/r', 'race', 'o/r', 'waiting')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO workflow_steps (workflow_id, seq, name, kind, depends_on, status) VALUES (1, 3, '人工评审', 'gate', '[]', 'waiting')",
            [],
        )
        .unwrap();
        // 一个改动待确认的任务
        db.execute(
            "INSERT INTO tasks (title, repo, status, detail) VALUES ('修欢迎页', 'o/r', 'changes', '改了 2 个文件')",
            [],
        )
        .unwrap();
        // 一个还没开工的任务
        db.execute(
            "INSERT INTO tasks (title, repo, status) VALUES ('修登录', 'o/r', 'pending')",
            [],
        )
        .unwrap();
        // 一个 PR 已开、等验收的任务（检查全绿）
        db.execute(
            "INSERT INTO tasks (title, repo, status, pr_number, pr_url, checks) \
             VALUES ('修部署', 'o/r', 'reviewing', 3, 'https://github.test/o/r/pull/3', ?1)",
            [r#"[{"name": "ci", "state": "success"}]"#],
        )
        .unwrap();
    }

    /// 四类待决策的事都聚到一处；点合并后任务自动完成
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aggregates_pending_decisions_and_merges() {
        let ts = test_state(Service::new(ServiceOptions::default()));
        let gh = fake_github().await;
        *ts.state.gh_api.lock().unwrap() = gh;
        ts.secrets.set("github_pat", "test-token").unwrap();
        seed(&ts);

        let inbox = view(&ts.state).unwrap();
        assert_eq!(inbox.gates.len(), 1);
        assert_eq!(inbox.gates[0].step_name, "人工评审");
        assert_eq!(inbox.changes.len(), 1);
        assert_eq!(inbox.changes[0].title, "修欢迎页");
        assert_eq!(inbox.starts.len(), 1);
        assert_eq!(inbox.starts[0].title, "修登录");
        assert_eq!(inbox.reviews.len(), 1);
        assert_eq!(inbox.reviews[0].checks.as_deref(), Some("success"));

        // 合并 PR → 任务完成，决策清空
        let task = merge_pr(&ts.state, 3).await.unwrap();
        assert_eq!(task.status, "done", "{task:?}");
        let inbox = view(&ts.state).unwrap();
        assert!(inbox.reviews.is_empty());
    }
}

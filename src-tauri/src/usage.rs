//! 用量与成本：每一轮对话的用量都落在 messages 表里，这里从各个维度汇总。
//! 不同 Agent 给的口径不一样（claude / grok / pi 给费用，codex 只给 token 数），
//! 所以费用与 token 分开统计，不互相折算。

use crate::AppState;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::State;

/// 一个分组的用量
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub name: String,
    pub turns: i64,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDay {
    /// `YYYY-MM-DD`（本地日期）
    pub day: String,
    pub turns: i64,
    pub cost_usd: f64,
    pub tokens: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub turns: i64,
    /// 有费用口径的轮次合计
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// 平均每轮耗时（毫秒）
    pub avg_duration_ms: i64,
    /// 最近 24 小时的费用
    pub last_24h_cost_usd: f64,
    pub last_24h_turns: i64,
    pub by_day: Vec<UsageDay>,
    pub by_agent: Vec<UsageBucket>,
    pub by_machine: Vec<UsageBucket>,
    /// 花得最多的几个会话
    pub top_sessions: Vec<UsageBucket>,
    /// 按仓库：手动任务、工作流、守护修复的花费都归到它的仓库上
    pub by_repo: Vec<UsageBucket>,
    /// 按自动化规则：规则触发的工作流（经 rule_runs 指回）里各步骤的花费
    pub by_rule: Vec<UsageBucket>,
    /// 今天（本地日）的花费
    pub today_cost_usd: f64,
    /// 每日预算（美元）；None 表示没设
    pub daily_budget_usd: Option<f64>,
}

/// 每日预算存在 settings 里的键（美元，小数点数字；空 / 0 表示不限）
const BUDGET_KEY: &str = "usage_daily_budget";

/// 读每日预算
pub fn budget_of(db: &Connection) -> Option<f64> {
    db.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        params![BUDGET_KEY],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|v| v.trim().parse::<f64>().ok())
    .filter(|v| *v > 0.0)
}

/// 写每日预算（0 或解析不了会被拒）
pub fn set_budget(db: &Connection, budget: f64) -> Result<(), String> {
    if !(budget.is_finite() && budget > 0.0) {
        return Err("预算要是大于 0 的数字（美元）".into());
    }
    db.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![BUDGET_KEY, format!("{budget:.4}")],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 今天（本地日）的全部 Agent 花费
pub fn today_cost(db: &Connection) -> f64 {
    db.query_row(
        &format!(
            "SELECT COALESCE(SUM(m.cost_usd), 0) {ASSISTANT}
             AND date(m.created_at, 'localtime') = date('now', 'localtime')"
        ),
        [],
        |row| row.get(0),
    )
    .unwrap_or(0.0)
}

/// 超预算了吗：返回 (已花, 预算)。没设预算或还没超就是 None。
/// 预算管的是这台 AutoAgents 今天**全部** Agent 花费——手动发起的会话不拦，
/// 拦的是自动化（规则触发、守护修复）不再自动开工。
pub fn over_budget(db: &Connection) -> Option<(f64, f64)> {
    let budget = budget_of(db)?;
    let spent = today_cost(db);
    (spent >= budget).then_some((spent, budget))
}

/// 只统计 Agent 的回答：用户消息与系统提示没有用量
const ASSISTANT: &str = "FROM messages m JOIN sessions s ON s.id = m.session_id WHERE m.role = 'assistant'";

fn buckets(
    db: &Connection,
    sql: &str,
    name: &dyn Fn(&rusqlite::Row) -> rusqlite::Result<String>,
) -> Result<Vec<UsageBucket>, String> {
    let mut stmt = db.prepare_cached(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(UsageBucket {
                name: name(row)?,
                turns: row.get(1)?,
                cost_usd: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

const BUCKET_COLS: &str = "COUNT(*), COALESCE(SUM(m.cost_usd), 0), COALESCE(SUM(m.input_tokens), 0), COALESCE(SUM(m.output_tokens), 0)";

fn machine_name(db: &Connection, node_id: &str) -> String {
    if node_id == "local" || node_id.is_empty() {
        return crate::nodes::LOCAL_NAME.to_string();
    }
    db.query_row(
        "SELECT name FROM nodes WHERE id = ?1",
        params![node_id],
        |row| row.get::<_, String>(0),
    )
    .unwrap_or_else(|_| node_id.to_string())
}

fn summary(db: &Connection, days: u32) -> Result<UsageSummary, String> {
    let days = days.clamp(1, 90);
    let (turns, cost_usd, input_tokens, output_tokens, avg_duration_ms): (i64, f64, i64, i64, i64) =
        db.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(m.cost_usd), 0), COALESCE(SUM(m.input_tokens), 0),
                        COALESCE(SUM(m.output_tokens), 0), CAST(COALESCE(AVG(m.duration_ms), 0) AS INTEGER)
                 {ASSISTANT}"
            ),
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?;
    let (last_24h_cost_usd, last_24h_turns): (f64, i64) = db
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(m.cost_usd), 0), COUNT(*)
                 {ASSISTANT} AND m.created_at >= datetime('now', '-1 day')"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())?;

    let mut by_day = buckets_day(db, days)?;
    by_day.sort_by(|a, b| a.day.cmp(&b.day));

    let by_agent = buckets(
        db,
        &format!(
            "SELECT s.adapter, {BUCKET_COLS} {ASSISTANT} GROUP BY s.adapter ORDER BY 3 DESC, 2 DESC"
        ),
        &|row| row.get(0),
    )?;
    let mut by_machine = buckets(
        db,
        &format!(
            "SELECT COALESCE(s.node_id, 'local'), {BUCKET_COLS} {ASSISTANT}
             GROUP BY COALESCE(s.node_id, 'local') ORDER BY 3 DESC, 2 DESC"
        ),
        &|row| row.get(0),
    )?;
    for bucket in &mut by_machine {
        bucket.name = machine_name(db, &bucket.name);
    }
    let by_repo = buckets(
        db,
        &format!(
            "SELECT COALESCE(t.repo, '（没有关联仓库）'), {BUCKET_COLS}
             FROM messages m JOIN sessions s ON s.id = m.session_id
             LEFT JOIN tasks t ON t.session_id = s.id
             WHERE m.role = 'assistant' GROUP BY t.repo ORDER BY 3 DESC LIMIT 20"
        ),
        &|row| row.get(0),
    )?;
    let by_rule = buckets(
        db,
        &format!(
            "SELECT r.name, {BUCKET_COLS}
             FROM messages m
             JOIN sessions s ON s.id = m.session_id
             JOIN tasks t ON t.session_id = s.id AND t.workflow_id IS NOT NULL
             JOIN workflows w ON w.id = t.workflow_id
             JOIN rule_runs rr ON rr.workflow_id = w.id
             JOIN rules r ON r.id = rr.rule_id
             WHERE m.role = 'assistant' GROUP BY r.id ORDER BY 3 DESC LIMIT 20"
        ),
        &|row| row.get(0),
    )?;
    let top_sessions = buckets(
        db,
        &format!(
            "SELECT s.title, {BUCKET_COLS} {ASSISTANT}
             GROUP BY m.session_id ORDER BY 3 DESC, 2 DESC LIMIT 8"
        ),
        &|row| row.get(0),
    )?;

    Ok(UsageSummary {
        turns,
        cost_usd,
        input_tokens,
        output_tokens,
        avg_duration_ms,
        last_24h_cost_usd,
        last_24h_turns,
        by_day,
        by_agent,
        by_machine,
        top_sessions,
        by_repo,
        by_rule,
        today_cost_usd: today_cost(db),
        daily_budget_usd: budget_of(db),
    })
}

/// 每天的用量：没有数据的日子也补一行，图表才是连续的。
/// 日期一律按本地时区归（表里存的是 UTC），换算交给 SQLite 的 `localtime`。
fn buckets_day(db: &Connection, days: u32) -> Result<Vec<UsageDay>, String> {
    // 最近这一天的起点（本地零点）往前推 days 天
    let since: String = db
        .query_row(
            "SELECT datetime(date('now', 'localtime', ?1), 'utc')",
            params![format!("-{} days", days as i64 - 1)],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let mut stmt = db
        .prepare_cached(
            "SELECT date(m.created_at, 'localtime') AS day, COUNT(*), COALESCE(SUM(m.cost_usd), 0),
                    COALESCE(SUM(m.input_tokens), 0) + COALESCE(SUM(m.output_tokens), 0)
             FROM messages m JOIN sessions s ON s.id = m.session_id
             WHERE m.role = 'assistant' AND m.created_at >= ?1
             GROUP BY day",
        )
        .map_err(|e| e.to_string())?;
    let found: HashMap<String, (i64, f64, i64)> = stmt
        .query_map(params![since], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get(1)?, row.get(2)?, row.get(3)?),
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut label = db
        .prepare_cached("SELECT date('now', 'localtime', ?1)")
        .map_err(|e| e.to_string())?;
    let mut list = Vec::with_capacity(days as usize);
    for back in (0..days).rev() {
        let day: String = label
            .query_row(params![format!("-{back} days")], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        let (turns, cost_usd, tokens) = found.get(&day).copied().unwrap_or_default();
        list.push(UsageDay {
            day,
            turns,
            cost_usd,
            tokens,
        });
    }
    Ok(list)
}

#[tauri::command]
pub fn usage_summary(
    state: State<'_, Arc<AppState>>,
    days: Option<u32>,
) -> Result<UsageSummary, String> {
    let db = state.db.lock().unwrap();
    summary(&db, days.unwrap_or(14))
}

/// 设每日预算（美元）。超了之后自动化（规则触发、守护修复）不再自动开工，只记事件
#[tauri::command]
pub fn usage_budget_set(state: State<'_, Arc<AppState>>, budget: f64) -> Result<(), String> {
    let db = state.db.lock().unwrap();
    set_budget(&db, budget)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::automation;
    use crate::secrets::SecretStore;
    use crate::testutil::{fake_agent, test_state, TestState};


    /// 直接往库里塞一条有花费的助手消息（今天，本地时区）
    fn seed_cost(db: &Connection, cost: f64, repo: Option<&str>, workflow: Option<i64>) {
        db.execute(
            "INSERT INTO sessions (adapter, title, node_id) VALUES ('claude-cli', 'seed', 'local')",
            [],
        )
        .unwrap();
        let session = db.last_insert_rowid();
        match (repo, workflow) {
            (Some(repo), Some(wf)) => {
                db.execute(
                    "INSERT INTO tasks (title, source, repo, session_id, workflow_id, status)
                     VALUES ('seed', 'manual', ?1, ?2, ?3, 'done')",
                    params![repo, session, wf],
                )
                .unwrap();
            }
            (Some(repo), None) => {
                db.execute(
                    "INSERT INTO tasks (title, source, repo, session_id, status)
                     VALUES ('seed', 'manual', ?1, ?2, 'done')",
                    params![repo, session],
                )
                .unwrap();
            }
            _ => {}
        }
        db.execute(
            "INSERT INTO messages (session_id, role, content, cost_usd, input_tokens, output_tokens, created_at)
             VALUES (?1, 'assistant', 'ok', ?2, 10, 5, datetime('now', 'localtime'))",
            params![session, cost],
        )
        .unwrap();
    }

    #[test]
    fn breakdowns_by_repo_and_rule() {
        let ts = test_state(fake_agent("true").1);
        let db = ts.state.db.lock().unwrap();
        // 规则 → 工作流 → 任务 → 会话 的链路，外加一条不关联任务的花费
        db.execute(
            "INSERT INTO rules (name, kind, repo, config) VALUES ('盯标签', 'issue-label', 'octo/demo', '{}')",
            [],
        )
        .unwrap();
        let rule = db.last_insert_rowid();
        db.execute(
            "INSERT INTO workflows (name, template, repo, status) VALUES ('修 Issue · octo/demo', 'issue-fix', 'octo/demo', 'done')",
            [],
        )
        .unwrap();
        let wf = db.last_insert_rowid();
        db.execute(
            "INSERT INTO rule_runs (rule_id, key, workflow_id) VALUES (?1, 'issue:7', ?2)",
            params![rule, wf],
        )
        .unwrap();
        seed_cost(&db, 0.06, Some("octo/demo"), Some(wf));
        seed_cost(&db, 0.02, Some("octo/other"), None);
        seed_cost(&db, 0.01, None, None);

        let summary = summary(&db, 14).unwrap();
        assert_eq!(summary.by_repo[0].name, "octo/demo");
        assert!((summary.by_repo[0].cost_usd - 0.06).abs() < 1e-9);
        assert!(
            summary
                .by_repo
                .iter()
                .any(|b| b.name == "octo/other" && (b.cost_usd - 0.02).abs() < 1e-9)
        );
        assert!(summary.by_repo.iter().any(|b| b.name.contains("没有关联仓库")));
        assert_eq!(summary.by_rule.len(), 1, "{:?}", summary.by_rule);
        assert_eq!(summary.by_rule[0].name, "盯标签");
        assert!((summary.by_rule[0].cost_usd - 0.06).abs() < 1e-9);
        drop(db);
    }

    #[tokio::test]
    async fn budget_gates_automation() {
        let ts = test_state(fake_agent("true").1);
        let state = ts.state.clone();
        {
            let db = state.db.lock().unwrap();
            assert!(over_budget(&db).is_none(), "没设预算不拦");
            set_budget(&db, 0.05).unwrap();
            assert!(over_budget(&db).is_none(), "还没花钱，不算超");
            seed_cost(&db, 0.06, None, None);
            db.execute(
                "INSERT INTO rules (name, kind, repo, config) VALUES ('盯标签', 'issue-label', 'octo/demo', '{}')",
                [],
            )
            .unwrap();
        }
        ts.secrets.set("github_pat", "t").unwrap();
        // 超了：这轮一个规则都不触发，只记一条事件
        assert_eq!(automation::tick(&state).await, 0);
        {
            let db = state.db.lock().unwrap();
            let mut stmt = db
                .prepare("SELECT level, message FROM events WHERE kind = 'usage.budget'")
                .unwrap();
            let rows: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(rows[0].0, "warning");
            assert!(
                rows[0].1.contains("$0.06") && rows[0].1.contains("$0.05"),
                "{}",
                rows[0].1
            );
        }
        // 再 tick 一遍：事件一天只记一条，不刷屏
        assert_eq!(automation::tick(&state).await, 0);
        let db = state.db.lock().unwrap();
        let n: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM events WHERE kind = 'usage.budget'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn over_budget_verdict() {
        let ts = test_state(fake_agent("true").1);
        let db = ts.state.db.lock().unwrap();
        set_budget(&db, 1.0).unwrap();
        seed_cost(&db, 1.5, Some("octo/demo"), None);
        assert_eq!(over_budget(&db), Some((1.5, 1.0)));
    }

    const OK: &str = r#"printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1"}' '{"type":"result","is_error":false,"result":"hello","total_cost_usd":0.02,"usage":{"input_tokens":5,"output_tokens":2}}'"#;

    async fn settle(state: &Arc<AppState>, id: i64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while state.live.lock().unwrap().contains_key(&id) {
            assert!(std::time::Instant::now() < deadline, "会话 #{id} 一直停在运行中");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    fn events(db: &Connection) -> Vec<(String, String, String, String)> {
        db.prepare("SELECT kind, level, source, link FROM events ORDER BY id")
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// 每一轮都留痕：开始、完成（或失败）写进活动流并链回会话；用量按天、按 Agent、按机器汇总
    #[tokio::test]
    async fn turns_are_audited_and_counted() {
        let service = fake_agent(OK).1;
        let TestState { state, .. } = test_state(service);
        let id = crate::chat::start(&state, None, "claude-cli", None, None, "audit me").await.unwrap();
        settle(&state, id).await;
        {
            let db = state.db.lock().unwrap();
            assert_eq!(
                events(&db),
                [
                    ("session.started".into(), "info".into(), "本机 · claude-cli".into(), "#/sessions?s=1".into()),
                    ("session.finished".into(), "info".into(), "本机 · claude-cli".into(), "#/sessions?s=1".into()),
                ]
            );
            let message: String = db
                .query_row("SELECT message FROM events WHERE kind = 'session.finished'", [], |row| row.get(0))
                .unwrap();
            assert!(message.contains("≈ $0.0200"), "{message}");
        }
        let usage = summary(&state.db.lock().unwrap(), 14).unwrap();
        assert_eq!((usage.turns, usage.last_24h_turns), (1, 1));
        assert!((usage.cost_usd - 0.02).abs() < 1e-9);
        assert_eq!((usage.input_tokens, usage.output_tokens), (5, 2));
        assert!(usage.avg_duration_ms >= 0);
        assert_eq!(usage.by_day.len(), 14);
        let today = usage.by_day.last().unwrap();
        assert_eq!((today.turns, today.cost_usd), (1, 0.02));
        assert!(usage.by_day[..13].iter().all(|day| day.turns == 0));
        assert_eq!(usage.by_agent[0].name, "claude-cli");
        assert_eq!(usage.by_machine[0].name, "本机");
        assert_eq!(usage.top_sessions[0].name, "audit me");

        // 没能启动的一轮：以失败事件留痕
        let broken = crate::testutil::service_for(std::path::PathBuf::from("/no/such/agent"), None);
        let TestState { state, .. } = test_state(broken);
        let id = crate::chat::start(&state, None, "codex-cli", None, None, "will fail").await.unwrap();
        settle(&state, id).await;
        let db = state.db.lock().unwrap();
        let kinds: Vec<String> = events(&db).into_iter().map(|row| row.0).collect();
        assert_eq!(kinds, ["session.started", "session.failed"]);
        let usage = summary(&db, 14).unwrap();
        assert_eq!(usage.turns, 0, "失败的一轮不算用量");
    }
}

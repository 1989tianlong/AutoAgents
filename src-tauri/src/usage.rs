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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::testutil::{fake_agent, test_state, TestState};

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

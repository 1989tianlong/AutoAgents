//! 任务流水线：Issue → 在独立的工作树里让 Agent 实现 → 人工确认 → 提交、推送、开 PR
//! → 跟踪 PR 与 CI → 没通过就让 Agent 修 → 合并。
//!
//! 几条原则：
//! - 每个任务一个工作树、一个分支：Agent 的改动不会碰到机器上正在用的那份代码
//! - Agent 只管改代码，不提交、不推送。往外发（提交、推送、开 PR）由这里做，
//!   而且默认要人先看过改动；任务的 `auto_pr` 打开时才做完直接发
//! - 推送用带校验的强推：分支在远端的样子必须和刚看到的一致，别人的提交不会被盖掉
//! - Agent 的全部往来都在这个任务的会话里（会话页可以旁听、插话）；每一步都记进活动流

use crate::AppState;
use crate::github;
use crate::nodes;
use crate::tasks::{self, Task};
use autoagents_core::service::GitOutput;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tauri::State;

/// 正在收尾（评估改动、提交）的任务：同一任务同时只走一条
#[derive(Default)]
pub struct Pipeline {
    busy: Mutex<HashSet<i64>>,
}

impl Pipeline {
    fn claim(&self, id: i64) -> bool {
        self.busy.lock().unwrap().insert(id)
    }

    fn release(&self, id: i64) {
        self.busy.lock().unwrap().remove(&id);
    }
}

/// 启动一个任务
#[derive(Clone, Debug, Default)]
pub struct StartOptions {
    /// 在哪台机器上做
    pub node_id: String,
    pub adapter: String,
    pub model: Option<String>,
    /// 仓库在那台机器上的位置
    pub workdir: String,
    /// 基于哪个分支；留空用仓库的默认分支
    pub base_branch: Option<String>,
    /// Agent 做完后不等确认，直接提交并开 PR
    pub auto_pr: bool,
}

/// 多久看一次各个 PR 的状态与检查
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
/// 单个文件的 diff 最多带回这么多字符
const DIFF_LIMIT: usize = 80_000;

fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max_chars).collect::<String>())
    }
}

/// `owner/repo` → (`owner`, `repo`)
pub fn split_repo(repo: &str) -> Result<(String, String), String> {
    let (owner, name) = repo
        .trim()
        .split_once('/')
        .ok_or_else(|| format!("仓库名应当是 owner/repo：{repo}"))?;
    Ok((owner.to_string(), name.to_string()))
}

// ---------------------------------------------------------------------------
// 在那台机器上执行 git
// ---------------------------------------------------------------------------

async fn git(
    state: &Arc<AppState>,
    node: &str,
    workdir: &str,
    args: &[&str],
) -> Result<GitOutput, String> {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let value = nodes::call(
        state,
        node,
        "git.run",
        &json!({ "workdir": workdir, "args": args }),
    )
    .await?;
    serde_json::from_value(value).map_err(|e| format!("解析 git 输出失败：{e}"))
}

/// 同 [`git`]，并要求退出码为 0；否则把 stderr 的尾部作为错误带出来
async fn git_ok(
    state: &Arc<AppState>,
    node: &str,
    workdir: &str,
    args: &[&str],
    what: &str,
) -> Result<String, String> {
    let out = git(state, node, workdir, args).await?;
    if out.ok() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "{what}失败：{}",
            github::clip_tail(out.stderr.trim(), 600)
        ))
    }
}

/// 那台机器上的一项 git 配置；没配、或不是仓库时为空
async fn git_config(state: &Arc<AppState>, node: &str, workdir: &str, key: &str) -> Option<String> {
    git(state, node, workdir, &["config", "--get", key])
        .await
        .ok()
        .filter(|out| out.ok())
        .map(|out| out.stdout.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 从远程地址里认出 GitHub 仓库：`git@github.com:o/r.git`、`https://github.com/o/r.git`……
fn parse_remote(url: &str) -> Option<(String, String)> {
    let lower = url.trim().trim_end_matches('/').to_ascii_lowercase();
    let at = lower.find("github.com")?;
    let rest = lower[at + "github.com".len()..].trim_start_matches([':', '/']);
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, name) = rest.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        None
    } else {
        Some((owner.to_string(), name.to_string()))
    }
}

/// 工作树放在仓库旁边的 `<仓库名>-wt/<分支名>` 里，一眼看得出是哪个任务的
fn worktree_path(repo_dir: &str, branch: &str) -> String {
    let repo_dir = Path::new(repo_dir);
    let name = repo_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = repo_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(repo_dir);
    parent
        .join(format!("{name}-wt"))
        .join(branch.replace('/', "-"))
        .to_string_lossy()
        .into_owned()
}

pub struct Prepared {
    pub repo_dir: String,
    pub worktree: String,
    pub base_sha: String,
}

/// 准备工作树：确认目录就是这个仓库、拉取最新的 base、从 `origin/<base>` 建出任务的分支。
/// 每次启动都先清掉上一次留下的工作树，保证从 base 干净起步。
pub async fn prepare_worktree(
    state: &Arc<AppState>,
    node: &str,
    workdir: &str,
    owner: &str,
    name: &str,
    base: &str,
    branch: &str,
) -> Result<Prepared, String> {
    let repo_dir = git_ok(
        state,
        node,
        workdir,
        &["rev-parse", "--show-toplevel"],
        "确认仓库位置",
    )
    .await?
    .trim()
    .to_string();
    let remote = git(state, node, &repo_dir, &["remote", "get-url", "origin"])
        .await
        .ok()
        .map(|out| out.stdout.trim().to_string())
        .unwrap_or_default();
    match parse_remote(&remote) {
        Some((o, n)) if o.eq_ignore_ascii_case(owner) && n.eq_ignore_ascii_case(name) => {}
        Some((o, n)) => return Err(format!("{repo_dir} 对应的是 {o}/{n}，不是 {owner}/{name}")),
        None if remote.is_empty() => return Err(format!("{repo_dir} 没有名为 origin 的远程仓库")),
        None => return Err(format!("{repo_dir} 的 origin（{remote}）不是 GitHub 仓库")),
    }
    git_ok(
        state,
        node,
        &repo_dir,
        &["fetch", "origin", base],
        &format!("拉取 origin/{base}"),
    )
    .await
    .map_err(|e| {
        format!("{e}\n（这台机器上有权限访问 {owner}/{name} 吗？分支 {base} 存在吗？）")
    })?;
    let worktree = worktree_path(&repo_dir, branch);
    let _ = git(
        state,
        node,
        &repo_dir,
        &["worktree", "remove", "--force", &worktree],
    )
    .await;
    git_ok(
        state,
        node,
        &repo_dir,
        &[
            "worktree",
            "add",
            // 不跟踪 origin/<base>：否则随手一推就可能推到 base 上去
            "--no-track",
            "-B",
            branch,
            &worktree,
            &format!("origin/{base}"),
        ],
        "创建工作树",
    )
    .await?;
    let base_sha = git_ok(
        state,
        node,
        &worktree,
        &["rev-parse", "HEAD"],
        "读取起点提交",
    )
    .await?
    .trim()
    .to_string();
    Ok(Prepared {
        repo_dir,
        worktree,
        base_sha,
    })
}

/// 把仓库克隆到那台机器上（机器上还没有这个仓库时用）
pub async fn clone_repo(
    state: &Arc<AppState>,
    node: &str,
    url: &str,
    dir: &str,
) -> Result<String, String> {
    let url = url.trim();
    let dir = dir.trim().trim_end_matches('/');
    if url.is_empty() || dir.is_empty() {
        return Err("请给出仓库地址和目标目录".into());
    }
    let parent = Path::new(dir)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());
    git_ok(state, node, &parent, &["clone", url, dir], "克隆仓库")
        .await
        .map_err(|e| {
            format!("{e}\n（那台机器上有权限访问这个仓库吗？SSH 地址需要它上面有对应的密钥）")
        })?;
    Ok(dir.to_string())
}

/// 删除任务的工作树和本地分支（任务删除时）。已在别处检出的分支删不掉，不算错
pub async fn clean_worktree(state: &Arc<AppState>, task: &Task) -> Result<(), String> {
    let (Some(node), Some(repo_dir), Some(worktree), Some(branch)) = (
        task.node_id.as_deref(),
        task.workdir.as_deref(),
        task.worktree.as_deref(),
        task.branch.as_deref(),
    ) else {
        return Ok(());
    };
    let _ = git(
        state,
        node,
        repo_dir,
        &["worktree", "remove", "--force", worktree],
    )
    .await;
    let _ = git(state, node, repo_dir, &["branch", "-D", branch]).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// 启动
// ---------------------------------------------------------------------------

/// 要做的事：Issue 的标题与正文（取不到正文时也照常开工），或手动任务的说明
async fn brief(state: &AppState, token: &str, task: &Task) -> String {
    if task.source == "manual" {
        return task
            .instructions
            .clone()
            .unwrap_or_else(|| "（没有需求说明）".to_string());
    }
    let Some(number) = task.issue_number else {
        return "（没有需求说明）".to_string();
    };
    let (owner, name) = task
        .repo
        .as_deref()
        .and_then(|repo| split_repo(repo).ok())
        .unwrap_or_default();
    match github::issue(state, token, &owner, &name, number).await {
        Ok(issue) => {
            let mut text = format!("# {}", issue.title);
            match issue.body {
                Some(body) => {
                    text.push_str("\n\n");
                    text.push_str(&clip(body.trim(), 12_000));
                }
                None => text.push_str("\n\n（Issue 没有正文）"),
            }
            text
        }
        Err(e) => format!("# {}\n\n（取不到 Issue 正文：{e}）", task.title),
    }
}

fn build_prompt(brief: &str, owner: &str, name: &str, branch: &str, base: &str) -> String {
    format!(
        "你在仓库 {owner}/{name} 的一个独立工作树里工作：当前分支 `{branch}`，基于 `{base}` 的最新提交。\
         现在目录里的内容就是这个分支。\n\n\
         ## 要做的事\n\n{brief}\n\n\
         ## 要求\n\n\
         - 只做和这件事直接相关的改动，不要顺手重构无关代码\n\
         - 改完运行相关的测试或检查；项目没有测试、或者在这台机器上跑不起来时，最后说明里写清楚验证了什么、没验证什么\n\
         - 不要提交、不要推送、不要开 PR：改完停下，AutoAgents 会把改动交给人工确认\n\
         - 不要改 CI 配置、锁文件和 `.git`，除非这件事本身就要求这样做\n\n\
         ## 完成时\n\n\
         最后用中文写一段简短总结：改了什么、为什么这样改、怎么验证的。这段话会成为 PR 描述的一部分。"
    )
}

/// 启动（或重新启动）一个任务：准备工作树，然后在这个任务专属的会话里把需求交给 Agent。
pub async fn start(
    state: &Arc<AppState>,
    task_id: i64,
    opts: &StartOptions,
) -> Result<Task, String> {
    let task = tasks::load(&state.db.lock().unwrap(), task_id)?;
    if task.status == "running" {
        return Err("任务正在进行中；要从头来先停止它".into());
    }
    if state.pipeline.busy.lock().unwrap().contains(&task_id) {
        return Err("任务正在收尾，稍等一下再启动".into());
    }
    let Some(repo) = task.repo.clone() else {
        return Err("这个任务没有关联仓库".into());
    };
    let (owner, name) = split_repo(&repo)?;
    if opts.adapter.trim().is_empty() {
        return Err("请选择用哪个 Agent".into());
    }
    if opts.workdir.trim().is_empty() {
        return Err("请给出仓库在那台机器上的位置".into());
    }
    let token = github::token_of(state)?;
    let branch = task
        .branch
        .clone()
        .unwrap_or_else(|| tasks::branch_name(task_id, &task.source, task.issue_number));
    let node = nodes::normalize(Some(&opts.node_id));

    let attempt = async {
        let base = match opts.base_branch.as_deref().map(str::trim) {
            Some(base) if !base.is_empty() => base.to_string(),
            _ => github::default_branch(state, &token, &owner, &name).await?,
        };
        let prepared =
            prepare_worktree(state, &node, opts.workdir.trim(), &owner, &name, &base, &branch).await?;
        let prompt = build_prompt(&brief(state, &token, &task).await, &owner, &name, &branch, &base);
        let session = crate::chat::start(
            state,
            Some(&node),
            opts.adapter.trim(),
            opts.model.as_deref(),
            Some(&prepared.worktree),
            &prompt,
        )
        .await?;
        // 会话已经在跑，这才登记任务：万一启动失败，任务直接记为失败，不会停在「进行中」
        let settled = {
            let db = state.db.lock().unwrap();
            db.execute(
                "UPDATE tasks SET node_id = ?1, adapter = ?2, model = ?3, workdir = ?4, worktree = ?5,
                        base_branch = ?6, base_sha = ?7, session_id = ?8, auto_pr = ?9,
                        head_sha = NULL, failure = NULL, pr_number = NULL, pr_url = NULL, checks = NULL,
                        status = 'running', detail = 'Agent 正在处理', updated_at = datetime('now')
                 WHERE id = ?10",
                params![
                    node,
                    opts.adapter.trim(),
                    opts.model,
                    prepared.repo_dir,
                    prepared.worktree,
                    base,
                    prepared.base_sha,
                    session,
                    opts.auto_pr as i64,
                    task_id,
                ],
            )
            .map_err(|e| e.to_string())?;
            let task = tasks::load(&db, task_id)?;
            tasks::record(&db, &task, "task.started", "info", "Agent 开始处理");
            // Agent 一瞬间就跑完的话，收尾事件在登记之前就过去了：现在补一次
            !state.live.lock().unwrap().contains_key(&session)
        };
        if settled {
            let ok = last_is_answer(state, session);
            after_turn(state, task_id, ok).await?;
        }
        Ok::<(), String>(())
    }
    .await;

    if let Err(e) = attempt {
        fail(state, task_id, &e);
    }
    tasks::load(&state.db.lock().unwrap(), task_id)
}

// ---------------------------------------------------------------------------
// 一轮结束之后
// ---------------------------------------------------------------------------

/// 会话里最后一条某种角色的消息；`role` 留空表示任意角色
fn last_message(state: &AppState, session_id: i64, role: Option<&str>) -> Option<(String, String)> {
    let db = state.db.lock().unwrap();
    let (sql, role) = match role {
        Some(role) => (
            "SELECT role, content FROM messages WHERE session_id = ?1 AND role = ?2 ORDER BY id DESC LIMIT 1",
            Some(role.to_string()),
        ),
        None => (
            "SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
            None,
        ),
    };
    db.query_row(sql, params![session_id, role], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })
    .optional()
    .ok()
    .flatten()
}

/// 会话里最后一条消息是不是 Agent 的回答（是：这一轮正常结束）
fn last_is_answer(state: &AppState, session_id: i64) -> bool {
    last_message(state, session_id, None).is_some_and(|(role, _)| role == "assistant")
}

fn fail(state: &Arc<AppState>, task_id: i64, reason: &str) {
    let db = state.db.lock().unwrap();
    if let Ok(task) = tasks::load(&db, task_id) {
        let _ = tasks::set_status(
            &db,
            task_id,
            "failed",
            Some("出错了，可以重试"),
            Some(reason),
        );
        tasks::record(&db, &task, "task.failed", "warning", &clip(reason, 160));
    }
}

/// 这一任务的会话里开始新一轮（用户在会话页继续追问，或流水线让它修 CI）：任务回到进行中
pub fn turn_started(state: &Arc<AppState>, session_id: i64) {
    let Some(task) = tasks::by_session(&state.db.lock().unwrap(), session_id) else {
        return;
    };
    // 已完成的任务不往回走：合并之后再聊只是普通对话
    if !matches!(
        task.status.as_str(),
        "running" | "changes" | "failed" | "reviewing"
    ) {
        return;
    }
    let db = state.db.lock().unwrap();
    let _ = tasks::set_status(&db, task.id, "running", Some("Agent 正在处理"), None);
    if task.status != "running" {
        tasks::record(&db, &task, "task.started", "info", "Agent 继续处理");
    }
}

/// 一轮结束：看看改动，决定下一步。收尾在后台做，会话这边不必等
pub fn turn_ended(state: &Arc<AppState>, session_id: i64, ok: bool) {
    let Some(task) = tasks::by_session(&state.db.lock().unwrap(), session_id) else {
        return;
    };
    let state = Arc::clone(state);
    tauri::async_runtime::spawn(async move {
        if let Err(e) = after_turn(&state, task.id, ok).await {
            fail(&state, task.id, &e);
        }
    });
}

async fn after_turn(state: &Arc<AppState>, task_id: i64, ok: bool) -> Result<(), String> {
    // 同一任务同时只评估一次：收尾事件和补发的那一次可能撞在一起
    if !state.pipeline.claim(task_id) {
        return Ok(());
    }
    let result = evaluate(state, task_id, ok).await;
    state.pipeline.release(task_id);
    result
}

async fn evaluate(state: &Arc<AppState>, task_id: i64, ok: bool) -> Result<(), String> {
    let task = tasks::load(&state.db.lock().unwrap(), task_id)?;
    if task.status != "running" {
        return Ok(());
    }
    if !ok {
        let reason = last_message(state, task.session_id.unwrap_or_default(), Some("system"))
            .map(|(_, content)| content)
            .unwrap_or_else(|| "这一轮没有完成".to_string());
        fail(
            state,
            task_id,
            &format!("这一轮没有完成：{}", clip(&reason, 600)),
        );
        return Ok(());
    }
    let diff = diff(state, &task).await?;
    if diff.entries.is_empty() {
        let reason =
            "Agent 完成了这一轮，但没有改动任何文件。可以到会话里让它接着做，或换个说法重试。";
        let db = state.db.lock().unwrap();
        let _ = tasks::set_status(&db, task_id, "failed", None, Some(reason));
        let task = tasks::load(&db, task_id)?;
        tasks::record(&db, &task, "task.failed", "warning", "没有改动任何文件");
        return Ok(());
    }
    if task.auto_pr {
        submit_inner(state, task_id).await
    } else {
        let db = state.db.lock().unwrap();
        let detail = format!("{} 个文件有改动，等你确认", diff.entries.len());
        tasks::set_status(&db, task_id, "changes", Some(&detail), None)?;
        let task = tasks::load(&db, task_id)?;
        tasks::record(&db, &task, "task.changes", "info", &detail);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 改动
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEntry {
    pub path: String,
    /// `M` 修改、`A` 新增、`D` 删除、`R` 重命名、`U` 冲突、`?` 未跟踪
    pub status: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffView {
    pub worktree: String,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    /// 比起点多出来的提交数
    pub ahead: i64,
    /// 已跟踪文件的增删行数汇总
    pub stat: String,
    pub entries: Vec<DiffEntry>,
}

/// 工作树里相对起点提交的改动
pub async fn diff(state: &Arc<AppState>, task: &Task) -> Result<DiffView, String> {
    let empty = DiffView {
        worktree: String::new(),
        branch: task.branch.clone(),
        base_branch: task.base_branch.clone(),
        ahead: 0,
        stat: String::new(),
        entries: Vec::new(),
    };
    let (Some(node), Some(worktree)) = (task.node_id.as_deref(), task.worktree.as_deref()) else {
        return Ok(empty);
    };
    let status = git_ok(
        state,
        node,
        worktree,
        &[
            "-c",
            "core.quotepath=false",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
        "读取改动",
    )
    .await?;
    let mut entries = Vec::new();
    for line in status.lines() {
        // 每行是两个状态字符、一个空格、路径
        if line.len() < 4 {
            continue;
        }
        let code = &line[..2];
        let path = line[3..].trim();
        // 重命名、拷贝写成 `旧 -> 新`
        let path = path.rsplit(" -> ").next().unwrap_or(path).to_string();
        let flag = if code.contains('?') {
            '?'
        } else {
            code.chars().find(|c| *c != ' ').unwrap_or('M')
        };
        entries.push(DiffEntry {
            path,
            status: flag.to_string(),
        });
    }
    let stat = git(state, node, worktree, &["diff", "--shortstat", "HEAD"])
        .await
        .ok()
        .map(|out| out.stdout.trim().to_string())
        .unwrap_or_default();
    let base = task
        .base_branch
        .as_deref()
        .map(|base| format!("origin/{base}"))
        .unwrap_or_else(|| "HEAD".to_string());
    let ahead = git(
        state,
        node,
        worktree,
        &["rev-list", "--count", &format!("{base}..HEAD")],
    )
    .await
    .ok()
    .and_then(|out| out.stdout.trim().parse::<i64>().ok())
    .unwrap_or(0);
    Ok(DiffView {
        worktree: worktree.to_string(),
        branch: task.branch.clone(),
        base_branch: task.base_branch.clone(),
        ahead,
        stat,
        entries,
    })
}

/// 工作树里某一个文件相对起点提交的改动（未跟踪的文件按整份新增算）
pub async fn diff_file(
    state: &Arc<AppState>,
    task: &Task,
    path: &str,
    untracked: bool,
) -> Result<String, String> {
    let (Some(node), Some(worktree)) = (task.node_id.as_deref(), task.worktree.as_deref()) else {
        return Err("这个任务还没有启动过".into());
    };
    let path = path.trim();
    if path.is_empty() || path.starts_with('-') {
        return Err("文件路径不对".into());
    }
    let out = if untracked {
        // `--no-index` 不认 `-C` 换过去的目录：路径要给绝对的
        let absolute = format!("{}/{}", worktree.trim_end_matches('/'), path);
        let out = git(
            state,
            node,
            worktree,
            &["diff", "--no-index", "--", "/dev/null", &absolute],
        )
        .await?;
        // 有差异时退出码是 1
        if out.ok() || out.code == Some(1) {
            out
        } else {
            return Err(format!("读取改动失败：{}", clip(out.stderr.trim(), 600)));
        }
    } else {
        let out = git(state, node, worktree, &["diff", "HEAD", "--", path]).await?;
        if !out.ok() {
            return Err(format!("读取改动失败：{}", clip(out.stderr.trim(), 600)));
        }
        out
    };
    // 二进制文件没有可读的 diff
    let mut text = out.stdout;
    if text.chars().count() > DIFF_LIMIT {
        text = format!(
            "{}\n…（内容太长，已截断）",
            text.chars().take(DIFF_LIMIT).collect::<String>()
        );
    }
    Ok(text)
}

// ---------------------------------------------------------------------------
// 提交、推送、开 PR
// ---------------------------------------------------------------------------

fn machine_name(state: &AppState, node: &str) -> String {
    if nodes::is_local(node) {
        nodes::LOCAL_NAME.to_string()
    } else {
        state
            .remote
            .view(node)
            .map(|view| view.name)
            .unwrap_or_else(|_| node.to_string())
    }
}

fn pr_body(task: &Task, summary: &str, machine: &str) -> String {
    let mut body = summary.trim().to_string();
    if body.is_empty() {
        body = "（Agent 没有留下总结）".to_string();
    }
    body.push_str("\n\n---\n\n");
    if let Some(number) = task.issue_number {
        body.push_str(&format!("Closes #{number}\n\n"));
    }
    body.push_str(&format!(
        "由 AutoAgents 的 {} 在「{machine}」上完成；分支 `{}` 基于 `{}`。",
        task.adapter.as_deref().unwrap_or("Agent"),
        task.branch.as_deref().unwrap_or_default(),
        task.base_branch.as_deref().unwrap_or_default(),
    ));
    body
}

/// 提交工作树里的改动、推送分支、开 PR（或把新提交推到已有的 PR 上）
async fn submit_inner(state: &Arc<AppState>, task_id: i64) -> Result<(), String> {
    let task = tasks::load(&state.db.lock().unwrap(), task_id)?;
    let (Some(node), Some(worktree), Some(branch), Some(repo), Some(base), Some(session)) = (
        task.node_id.clone(),
        task.worktree.clone(),
        task.branch.clone(),
        task.repo.clone(),
        task.base_branch.clone(),
        task.session_id,
    ) else {
        return Err("这个任务还没有启动过".into());
    };
    let (owner, name) = split_repo(&repo)?;
    let token = github::token_of(state)?;
    if diff(state, &task).await?.entries.is_empty() {
        return Err("工作树里没有改动，没有可提交的内容".into());
    }
    let first_pr = task.pr_number.is_none();
    {
        let db = state.db.lock().unwrap();
        tasks::set_status(&db, task_id, &task.status, Some("正在提交并创建 PR…"), None)?;
    }

    // 提交身份：那台机器上没配的话，用 GitHub 账号的 noreply 地址（不改它的 git 配置）
    let user_name = git_config(state, &node, &worktree, "user.name").await;
    let user_email = git_config(state, &node, &worktree, "user.email").await;
    let mut identity: Vec<String> = Vec::new();
    if user_name.is_none() || user_email.is_none() {
        let (login, id) = github::whoami(state, &token).await?;
        if user_name.is_none() {
            identity.push(format!("user.name={login}"));
        }
        if user_email.is_none() {
            identity.push(format!("user.email={id}+{login}@users.noreply.github.com"));
        }
    }

    git_ok(state, &node, &worktree, &["add", "--all"], "暂存改动").await?;
    let message = if first_pr {
        let mut text = task.title.clone();
        if let Some(number) = task.issue_number {
            text.push_str(&format!("\n\nRefs #{number}"));
        }
        text
    } else {
        format!(
            "fix: 回应 PR #{} 上没通过的检查",
            task.pr_number.unwrap_or_default()
        )
    };
    let mut commit: Vec<String> = Vec::new();
    for value in &identity {
        commit.push("-c".into());
        commit.push(value.clone());
    }
    commit.extend(["commit".into(), "-m".into(), message]);
    let commit: Vec<&str> = commit.iter().map(String::as_str).collect();
    git_ok(state, &node, &worktree, &commit, "提交改动").await?;

    // 推送。用带校验的强推：远端的分支必须还是刚看到的那个样子（不存在，或是上次留下的提交），
    // 别人的提交不会被盖掉
    let remote_now = git(
        state,
        &node,
        &worktree,
        &["ls-remote", "origin", &format!("refs/heads/{branch}")],
    )
    .await?
    .stdout
    .split_whitespace()
    .next()
    .unwrap_or_default()
    .to_string();
    git_ok(
        state,
        &node,
        &worktree,
        &[
            "push",
            "--set-upstream",
            "origin",
            &format!("{branch}:{branch}"),
            &format!("--force-with-lease=refs/heads/{branch}:{remote_now}"),
        ],
        "推送分支",
    )
    .await
    .map_err(|e| format!("{e}\n（这台机器上的 git 有权限推送到 {owner}/{name} 吗？）"))?;
    let head = git_ok(state, &node, &worktree, &["rev-parse", "HEAD"], "读取提交")
        .await?
        .trim()
        .to_string();

    // PR 描述用 Agent 自己写的总结
    let summary = last_message(state, session, Some("assistant"))
        .map(|(_, content)| content)
        .unwrap_or_default();
    let detail = if first_pr {
        let pr = github::create_pr(
            state,
            &token,
            &owner,
            &name,
            &task.title,
            &branch,
            &base,
            &pr_body(&task, &summary, &machine_name(state, &node)),
        )
        .await?;
        let db = state.db.lock().unwrap();
        db.execute(
            "UPDATE tasks SET pr_number = ?1, pr_url = ?2 WHERE id = ?3",
            params![pr.number, pr.html_url, task_id],
        )
        .map_err(|e| e.to_string())?;
        let task = tasks::load(&db, task_id)?;
        tasks::record(
            &db,
            &task,
            "task.pr",
            "info",
            &format!("已创建 PR #{}", pr.number),
        );
        format!("已创建 PR #{}，等审查", pr.number)
    } else {
        // 已有的 PR：把这次的说明追加进去。追加不上不影响任务
        let addition = format!("\n\n---\n\n### 后续修改\n\n{}", clip(summary.trim(), 4_000));
        let number = task.pr_number.unwrap_or_default();
        let _ = github::append_pr_body(state, &token, &owner, &name, number, &addition).await;
        let db = state.db.lock().unwrap();
        let task = tasks::load(&db, task_id)?;
        tasks::record(&db, &task, "task.push", "info", "已把新的提交推到 PR 上");
        format!("已推送新的提交到 PR #{number}")
    };
    {
        let db = state.db.lock().unwrap();
        db.execute(
            "UPDATE tasks SET head_sha = ?1 WHERE id = ?2",
            params![head, task_id],
        )
        .map_err(|e| e.to_string())?;
        tasks::set_status(&db, task_id, "reviewing", Some(&detail), None)?;
    }
    let _ = refresh(state, task_id).await;
    Ok(())
}

/// 人工确认改动之后：提交、推送、开 PR
pub async fn submit(state: &Arc<AppState>, task_id: i64) -> Result<Task, String> {
    let task = tasks::load(&state.db.lock().unwrap(), task_id)?;
    if !matches!(task.status.as_str(), "changes" | "reviewing" | "failed") {
        return Err("现在没有等你确认的改动".into());
    }
    if !state.pipeline.claim(task_id) {
        return Err("任务正在收尾，稍等一下再试".into());
    }
    let result = submit_inner(state, task_id).await;
    state.pipeline.release(task_id);
    if let Err(e) = result {
        fail(state, task_id, &e);
    }
    tasks::load(&state.db.lock().unwrap(), task_id)
}

// ---------------------------------------------------------------------------
// PR 与 CI
// ---------------------------------------------------------------------------

/// 取一次 PR 的状态与检查，记到任务上
pub async fn refresh(state: &Arc<AppState>, task_id: i64) -> Result<Task, String> {
    let task = tasks::load(&state.db.lock().unwrap(), task_id)?;
    let Some(number) = task.pr_number else {
        return Ok(task);
    };
    let repo = task.repo.clone().ok_or("这个任务没有关联仓库")?;
    let (owner, name) = split_repo(&repo)?;
    let token = github::token_of(state)?;
    let pr = github::pr_state(state, &token, &owner, &name, number).await?;
    let summary = if pr.checks.is_empty() {
        "还没有检查上报".to_string()
    } else {
        format!(
            "{} 项通过，{} 项进行中，{} 项失败",
            pr.success, pr.pending, pr.failure
        )
    };
    {
        let db = state.db.lock().unwrap();
        db.execute(
            "UPDATE tasks SET checks = ?1, head_sha = ?2 WHERE id = ?3",
            params![json!(pr.checks).to_string(), pr.head_sha, task_id],
        )
        .map_err(|e| e.to_string())?;
        if pr.merged {
            let detail = format!("PR #{number} 已合并");
            tasks::set_status(&db, task_id, "done", Some(&detail), None)?;
            let task = tasks::load(&db, task_id)?;
            tasks::record(&db, &task, "task.merged", "info", &detail);
        } else if pr.state == "closed" {
            let reason = format!("PR #{number} 被关闭了，没有合并");
            tasks::set_status(&db, task_id, "failed", None, Some(&reason))?;
            let task = tasks::load(&db, task_id)?;
            tasks::record(&db, &task, "task.failed", "warning", &reason);
        } else if task.status == "reviewing" {
            db.execute(
                "UPDATE tasks SET detail = ?1 WHERE id = ?2",
                params![format!("PR #{number} 等审查：{summary}"), task_id],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    // PR 合并了：开了「合并即部署」的部署目标自动发起部署（网络请求，放在锁外）
    if pr.merged && !pr.base_branch.is_empty() {
        let sha = pr.merge_commit_sha.clone().or(Some(pr.head_sha.clone()));
        crate::deploy::on_pr_merged(state, &repo, &pr.base_branch, sha.as_deref()).await;
    }
    tasks::load(&state.db.lock().unwrap(), task_id)
}

/// 让 Agent 修 CI：把没通过的检查和日志交给它，在同一个会话里继续
pub async fn fix(state: &Arc<AppState>, task_id: i64) -> Result<(), String> {
    let task = tasks::load(&state.db.lock().unwrap(), task_id)?;
    let session = task.session_id.ok_or("这个任务没有会话")?;
    let repo = task.repo.clone().ok_or("这个任务没有关联仓库")?;
    let (owner, name) = split_repo(&repo)?;
    let token = github::token_of(state)?;
    let failed: Vec<String> = task
        .checks
        .as_ref()
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter(|check| check["state"] == "failure")
                .filter_map(|check| check["name"].as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let head = task.head_sha.clone().unwrap_or_default();
    let logs = github::failed_check_logs(state, &token, &owner, &name, &head)
        .await
        .unwrap_or_default();
    let mut text = format!(
        "PR #{} 上的检查没有通过。请修复下面这些检查指出的问题：不要绕过或删除测试，\
         改完说明原因。改完同样不要提交。\n\n## 没通过的检查\n",
        task.pr_number.unwrap_or_default()
    );
    if logs.is_empty() {
        for name in &failed {
            text.push_str(&format!(
                "\n### {name}\n\n（取不到日志：可能还在跑，或者这一项没有日志。请到 PR 页面查看。）"
            ));
        }
    }
    for (name, log) in &logs {
        text.push_str(&format!("\n### {name}\n\n```\n{log}\n```\n"));
    }
    {
        let db = state.db.lock().unwrap();
        tasks::set_status(
            &db,
            task_id,
            "running",
            Some("Agent 正在修复检查指出的问题"),
            None,
        )?;
        let task = tasks::load(&db, task_id)?;
        tasks::record(
            &db,
            &task,
            "task.started",
            "info",
            "让 Agent 修没通过的检查",
        );
    }
    // 发不出去（比如上一轮还没停）：任务退回原状，不算失败
    if let Err(e) = crate::chat::send(state, session, &text).await {
        let db = state.db.lock().unwrap();
        let _ = tasks::set_status(&db, task_id, &task.status, task.detail.as_deref(), None);
        return Err(e);
    }
    Ok(())
}

/// 停止任务正在跑的这一轮
pub async fn abort(state: &Arc<AppState>, task_id: i64) -> Result<(), String> {
    let task = tasks::load(&state.db.lock().unwrap(), task_id)?;
    let session = task.session_id.ok_or("这个任务没有正在运行的一轮")?;
    crate::chat::stop(state, session).await
}

/// 刷新所有开着 PR 的任务
pub async fn refresh_all(state: &Arc<AppState>) -> Result<usize, String> {
    let tasks = tasks::in_status(&state.db.lock().unwrap(), "reviewing")?;
    let mut count = 0;
    for task in tasks {
        if task.pr_number.is_some() && refresh(state, task.id).await.is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

/// 应用启动后被中断的任务补做一次收尾：应用退出时本机的一轮已经不在了，
/// 远程机器上的在断线时被停掉。按会话里最后一条消息判断那一轮是否正常结束。
async fn recover(state: &Arc<AppState>) {
    let Ok(running) = tasks::in_status(&state.db.lock().unwrap(), "running") else {
        return;
    };
    for task in running {
        let Some(session) = task.session_id else {
            continue;
        };
        if state.live.lock().unwrap().contains_key(&session) {
            continue;
        }
        let ok = last_is_answer(state, session);
        if let Err(e) = after_turn(state, task.id, ok).await {
            fail(state, task.id, &e);
        }
    }
}

/// 应用启动后调用：补做被中断的收尾，然后定期刷新各个 PR
pub fn start_polling(state: Arc<AppState>) {
    tauri::async_runtime::spawn(async move {
        recover(&state).await;
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let _ = refresh_all(&state).await;
        }
    });
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn task_start(
    state: State<'_, Arc<AppState>>,
    id: i64,
    node_id: String,
    adapter: String,
    model: Option<String>,
    workdir: String,
    base_branch: Option<String>,
    auto_pr: Option<bool>,
) -> Result<Task, String> {
    start(
        &state,
        id,
        &StartOptions {
            node_id,
            adapter,
            model,
            workdir,
            base_branch,
            auto_pr: auto_pr.unwrap_or(false),
        },
    )
    .await
}

#[tauri::command]
pub async fn task_submit(state: State<'_, Arc<AppState>>, id: i64) -> Result<Task, String> {
    submit(&state, id).await
}

#[tauri::command]
pub async fn task_fix(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    fix(&state, id).await
}

#[tauri::command]
pub async fn task_refresh(state: State<'_, Arc<AppState>>, id: i64) -> Result<Task, String> {
    refresh(&state, id).await
}

#[tauri::command]
pub async fn tasks_refresh(state: State<'_, Arc<AppState>>) -> Result<usize, String> {
    refresh_all(&state).await
}

#[tauri::command]
pub async fn task_abort(state: State<'_, Arc<AppState>>, id: i64) -> Result<(), String> {
    abort(&state, id).await
}

#[tauri::command]
pub async fn task_diff(state: State<'_, Arc<AppState>>, id: i64) -> Result<DiffView, String> {
    let task = tasks::load(&state.db.lock().unwrap(), id)?;
    diff(&state, &task).await
}

#[tauri::command]
pub async fn task_diff_file(
    state: State<'_, Arc<AppState>>,
    id: i64,
    path: String,
    untracked: Option<bool>,
) -> Result<String, String> {
    let task = tasks::load(&state.db.lock().unwrap(), id)?;
    diff_file(&state, &task, &path, untracked.unwrap_or(false)).await
}

#[tauri::command]
pub async fn repo_clone(
    state: State<'_, Arc<AppState>>,
    node_id: String,
    url: String,
    dir: String,
) -> Result<String, String> {
    let node = nodes::normalize(Some(&node_id));
    clone_repo(&state, &node, &url, &dir).await
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::chat;
    use crate::secrets::SecretStore;
    use crate::testutil::{
        Req, TestState, fake_script, git_in, read_request, respond, service_for, setup_repo,
        test_state,
    };

    /// 假的 GitHub API：只实现流水线会调的那几个接口，并把收到的请求记下来
    struct FakeGithub {
        addr: String,
        /// (方法, 路径, 请求体)
        calls: Arc<Mutex<Vec<(String, String, String)>>>,
        /// PR 头部提交上的检查：(名字, 是否失败)
        checks: Arc<Mutex<Vec<(String, bool)>>>,
        merged: Arc<Mutex<bool>>,
    }

    /// 起一个假的 GitHub API
    async fn start_github() -> FakeGithub {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let fake = FakeGithub {
            addr,
            calls: Arc::default(),
            checks: Arc::default(),
            merged: Arc::default(),
        };
        let (calls, checks, merged) = (
            Arc::clone(&fake.calls),
            Arc::clone(&fake.checks),
            Arc::clone(&fake.merged),
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let (calls, checks, merged) =
                    (Arc::clone(&calls), Arc::clone(&checks), Arc::clone(&merged));
                tokio::spawn(async move {
                    let Req {
                        method, path, body, ..
                    } = read_request(&mut stream).await;
                    calls
                        .lock()
                        .unwrap()
                        .push((method.clone(), path.clone(), body.clone()));
                    // 回一段 JSON
                    macro_rules! json {
                        ($text:expr) => {
                            respond(&mut stream, "200 OK", "application/json", $text)
                        };
                    }
                    // 路径里带查询串的先比前缀
                    let route = path.split('?').next().unwrap_or("").to_string();
                    match (method.as_str(), route.as_str()) {
                        ("GET", "/repos/o/r") => json!(r#"{"default_branch": "main"}"#).await,
                        ("GET", "/repos/o/r/issues/7") => json!(
                            r#"{"number": 7, "title": "加一句问候", "body": "在 README 里加一句问候。", "html_url": "https://github.test/o/r/issues/7"}"#
                        )
                        .await,
                        ("GET", "/user") => json!(r#"{"login": "octo", "id": 42}"#).await,
                        ("POST", "/repos/o/r/pulls") => {
                            if body.contains("agent/issue-8") {
                                json!(r#"{"message": "Validation Failed", "errors": [{"message": "A pull request already exists for octo:agent/issue-8."}]}"#).await;
                            } else {
                                json!(r#"{"number": 1, "html_url": "https://github.test/o/r/pull/1", "head": {"sha": "abc123"}, "base": {"ref": "main"}, "title": "加一句问候", "state": "open"}"#).await;
                            }
                        }
                        ("GET", "/repos/o/r/pulls") => {
                            json!(r#"[{"number": 1, "html_url": "https://github.test/o/r/pull/1", "head": {"sha": "abc123"}, "base": {"ref": "main"}}]"#).await
                        }
                        ("GET", "/repos/o/r/pulls/1") => {
                            let merged = *merged.lock().unwrap();
                            let text = format!(
                                r#"{{"number": 1, "html_url": "https://github.test/o/r/pull/1", "head": {{"sha": "abc123"}}, "state": "{}", "merged": {merged}, "merged_at": {}}}"#,
                                if merged { "closed" } else { "open" },
                                if merged { r#""2026-11-24T00:00:00Z""# } else { "null" }
                            );
                            json!(&text).await
                        }
                        ("PATCH", "/repos/o/r/pulls/1") => json!(r#"{"number": 1}"#).await,
                        ("GET", "/repos/o/r/commits/abc123/status") => json!(r#"{"state": "pending", "statuses": []}"#).await,
                        ("GET", "/repos/o/r/commits/abc123/check-runs") => {
                            let runs: Vec<String> = checks
                                .lock()
                                .unwrap()
                                .iter()
                                .map(|(name, failed)| {
                                    format!(
                                        r#"{{"name": "{name}", "status": "completed", "conclusion": "{}", "html_url": "https://github.test/o/r/actions/runs/1", "details_url": "https://github.test/o/r/actions/runs/1/job/99"}}"#,
                                        if *failed { "failure" } else { "success" }
                                    )
                                })
                                .collect();
                            json!(&format!(r#"{{"check_runs": [{}]}}"#, runs.join(","))).await
                        }
                        ("GET", "/repos/o/r/actions/jobs/99/logs") => {
                            respond(&mut stream, "200 OK", "text/plain", "--- test\nFAILED src/greet.test.ts\nExpected 'hi' but got 'bye'\n").await
                        }
                        _ => json!(r#"{"message": "Not Found"}"#).await,
                    }
                });
            }
        });
        fake
    }

    /// 冒充 Agent：第一轮写一个文件；让它修 CI 的那一轮（提示词里有「检查没有通过」）改另一个文件
    const AGENT: &str = r#"case "$*" in
  *检查没有通过*) echo fixed > fix.txt ;;
  *) echo hello > greet.txt ;;
esac
printf '%s\n' '{"type":"result","is_error":false,"result":"新增了问候语，并补了测试。"}'"#;

    async fn wait_status(state: &Arc<AppState>, id: i64, want: &[&str]) -> Task {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(40);
        loop {
            let task = tasks::load(&state.db.lock().unwrap(), id).unwrap();
            if want.contains(&task.status.as_str()) {
                return task;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "任务一直停在 {}：{}",
                task.status,
                task.detail.as_deref().unwrap_or("")
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    fn calls(fake: &FakeGithub, method: &str, prefix: &str) -> Vec<(String, String)> {
        fake.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == method && p.starts_with(prefix))
            .map(|(_, p, b)| (p.clone(), b.clone()))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn issue_to_merged_pr_through_a_real_worktree() {
        // GitHub 接口都打到本地的假服务器上
        let github = start_github().await;
        let tmp = tempfile::tempdir().unwrap();
        let workdir = setup_repo(tmp.path());
        let TestState { state, secrets, .. } =
            test_state(service_for(fake_script("claude", AGENT), None));
        *state.gh_api.lock().unwrap() = github.addr.clone();
        secrets.set("github_pat", "test-token").unwrap();
        let db = state.db.lock().unwrap();
        let issue_task = tasks::create_issue(
            &db,
            "加一句问候",
            "o/r",
            7,
            "https://github.test/o/r/issues/7",
        )
        .unwrap()
        .id;
        drop(db);
        let opts = StartOptions {
            adapter: "claude-cli".into(),
            workdir: workdir.to_string_lossy().into_owned(),
            ..Default::default()
        };

        // ---------- 1. 启动：独立工作树、Issue 正文进了提示词、改完停下等人确认 ----------
        let task = start(&state, issue_task, &opts).await.unwrap();
        assert_eq!(task.status, "running", "{task:?}");
        assert_eq!(task.base_branch.as_deref(), Some("main"));
        assert!(
            task.worktree
                .as_deref()
                .unwrap()
                .ends_with("app-wt/agent-issue-7"),
            "{}",
            task.worktree.as_deref().unwrap_or("")
        );
        let worktree = std::path::PathBuf::from(task.worktree.clone().unwrap());
        assert!(worktree.is_dir(), "工作树要真的建出来");
        let task = wait_status(&state, issue_task, &["changes", "failed"]).await;
        assert_eq!(
            task.status,
            "changes",
            "{}",
            task.failure.as_deref().unwrap_or("")
        );
        let changes = diff(&state, &task).await.unwrap();
        assert_eq!(changes.entries.len(), 1, "{changes:?}");
        assert_eq!(
            (
                changes.entries[0].path.as_str(),
                changes.entries[0].status.as_str()
            ),
            ("greet.txt", "?")
        );
        let patch = diff_file(&state, &task, "greet.txt", true)
            .await
            .unwrap_or_else(|e| panic!("读取未跟踪文件的改动失败：{e}"));
        assert!(patch.contains("+hello"), "{patch}");
        // 机器上正在用的那份代码没被动过
        assert!(!workdir.join("greet.txt").exists());
        // Issue 正文进了给 Agent 的提示词
        let prompt: String = state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT content FROM messages WHERE session_id = ?1 AND role = 'user' ORDER BY id LIMIT 1",
                params![task.session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(prompt.contains("在 README 里加一句问候"), "{prompt}");
        assert!(prompt.contains("不要提交"), "{prompt}");

        // ---------- 2. 在会话里继续追问：任务回到进行中，改完再等确认 ----------
        chat::send(&state, task.session_id.unwrap(), "再改一下")
            .await
            .unwrap();
        assert_eq!(
            tasks::load(&state.db.lock().unwrap(), issue_task)
                .unwrap()
                .status,
            "running"
        );
        let task = wait_status(&state, issue_task, &["changes", "failed"]).await;
        assert_eq!(task.status, "changes");

        // ---------- 3. 确认之后：提交、推送、开 PR ----------
        // PR 头部提交上的检查：一项通过、一项失败（提交之后立刻取一次）
        github.checks.lock().unwrap().push(("build".into(), false));
        github.checks.lock().unwrap().push(("test".into(), true));
        let task = submit(&state, issue_task).await.unwrap();
        assert_eq!(
            task.status,
            "reviewing",
            "{}",
            task.failure.as_deref().unwrap_or("")
        );
        assert_eq!(task.pr_number, Some(1));
        assert_eq!(task.head_sha.as_deref(), Some("abc123"));
        let log = git_in(
            &worktree,
            &["log", "--format=%an <%ae> %B", "origin/main..HEAD"],
        )
        .stdout;
        let log = String::from_utf8_lossy(&log);
        // 提交身份：那台机器上配了就用它的（这里读到什么，提交里就该是什么）；
        // 没配的话用 GitHub 账号的 noreply 地址
        let configured = std::process::Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["config", "--get", "user.email"])
            .output()
            .unwrap();
        let expected_email = if configured.status.success() {
            String::from_utf8_lossy(&configured.stdout)
                .trim()
                .to_string()
        } else {
            "42+octo@users.noreply.github.com".to_string()
        };
        assert!(
            log.contains(&expected_email),
            "提交身份应当是 {expected_email}：{log}"
        );
        assert!(log.contains("Refs #7"), "{log}");
        assert_eq!(
            git_in(&worktree, &["status", "--porcelain"]).stdout.len(),
            0,
            "提交之后工作树是干净的"
        );
        let (_, body) = calls(&github, "POST", "/repos/o/r/pulls").remove(0);
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (body["head"].as_str(), body["base"].as_str()),
            (Some("agent/issue-7"), Some("main"))
        );
        assert!(body["body"].as_str().unwrap().contains("Closes #7"));
        assert!(body["body"].as_str().unwrap().contains("新增了问候语"));
        // 检查已经取回来了：一项通过、一项失败
        assert_eq!(
            task.checks.as_ref().and_then(Value::as_array).map(Vec::len),
            Some(2)
        );
        let detail = task.detail.as_deref().unwrap_or("");
        assert!(
            detail.contains("1 项通过") && detail.contains("1 项失败"),
            "{detail}"
        );
        let kinds: Vec<String> = {
            let db = state.db.lock().unwrap();
            let mut stmt = db
                .prepare("SELECT kind FROM events WHERE kind LIKE 'task.%' ORDER BY id")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .flatten()
                .collect()
        };
        assert_eq!(
            kinds,
            [
                "task.created",
                "task.started",
                "task.changes",
                "task.started",
                "task.changes",
                "task.pr"
            ]
        );

        // ---------- 4. CI 没通过：把日志交给 Agent 修，改完再推到同一个 PR 上 ----------
        fix(&state, issue_task).await.unwrap();
        assert_eq!(
            tasks::load(&state.db.lock().unwrap(), issue_task)
                .unwrap()
                .status,
            "running"
        );
        let task = wait_status(&state, issue_task, &["changes", "failed"]).await;
        assert_eq!(task.status, "changes");
        assert!(worktree.join("fix.txt").exists(), "Agent 应当改了 fix.txt");
        // 修的那一轮里拿到了失败的日志
        let fix_prompt: String = state
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT content FROM messages WHERE session_id = ?1 AND role = 'user' ORDER BY id DESC LIMIT 1",
                params![task.session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            fix_prompt.contains("FAILED src/greet.test.ts"),
            "{fix_prompt}"
        );
        let task = submit(&state, issue_task).await.unwrap();
        assert_eq!(task.status, "reviewing");
        let pushed = git_in(
            &worktree,
            &["rev-list", "--count", "origin/main..origin/agent/issue-7"],
        );
        assert_eq!(
            String::from_utf8_lossy(&pushed.stdout).trim(),
            "2",
            "远端的分支上要有两个提交"
        );
        let (_, body) = calls(&github, "PATCH", "/repos/o/r/pulls/1").remove(0);
        assert!(body.contains("后续修改"), "{body}");

        // ---------- 5. 合并 ----------
        *github.merged.lock().unwrap() = true;
        let task = refresh(&state, issue_task).await.unwrap();
        assert_eq!(
            task.status,
            "done",
            "{}",
            task.detail.as_deref().unwrap_or("")
        );

        // ---------- 6. 做完直接发（不等确认）的手动任务 ----------
        *github.merged.lock().unwrap() = false;
        let manual_task = tasks::create_manual(
            &state.db.lock().unwrap(),
            "补一份 CHANGELOG",
            "o/r",
            "写一份 CHANGELOG。",
        )
        .unwrap()
        .id;
        let task = start(
            &state,
            manual_task,
            &StartOptions {
                auto_pr: true,
                ..opts.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(task.status, "running");
        assert!(task.instructions.as_deref().unwrap().contains("CHANGELOG"));
        let task = wait_status(&state, manual_task, &["reviewing", "failed"]).await;
        assert_eq!(
            task.status,
            "reviewing",
            "{}",
            task.failure.as_deref().unwrap_or("")
        );
        let created = calls(&github, "POST", "/repos/o/r/pulls");
        assert!(
            created[1].1.contains(r#""head":"agent/task-2""#),
            "{}",
            created[1].1
        );
        assert!(
            !created[1].1.contains("Closes"),
            "手动任务的 PR 不该关联 Issue：{}",
            created[1].1
        );
    }

    /// 认仓库地址、排工作树的位置
    #[test]
    fn remote_and_worktree_names() {
        assert_eq!(
            parse_remote("git@github.com:1989tianlong/AutoAgents.git"),
            Some(("1989tianlong".into(), "autoagents".into()))
        );
        assert_eq!(
            parse_remote("https://github.com/1989tianlong/AutoAgents"),
            Some(("1989tianlong".into(), "autoagents".into()))
        );
        assert_eq!(
            parse_remote("https://user:secret@github.com/1989tianlong/AutoAgents.git/"),
            Some(("1989tianlong".into(), "autoagents".into()))
        );
        assert_eq!(parse_remote("git@gitlab.com:a/b.git"), None);
        assert_eq!(parse_remote("/srv/repos/app"), None);
        assert_eq!(
            worktree_path("/Users/me/code/AutoAgents", "agent/issue-12"),
            "/Users/me/code/AutoAgents-wt/agent-issue-12"
        );
        assert_eq!(
            worktree_path("/repo", "agent/task-3"),
            "/repo-wt/agent-task-3"
        );
        assert!(github::clip_tail("abc", 10).starts_with("abc"));
        let long = "l1\nl2\nl3";
        assert_eq!(github::clip_tail(long, 4), "…（前面省略）\nl3");
    }

    /// 启动不了的任务：原因留在任务上，不会停在「进行中」
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failures_are_recorded_on_the_task() {
        let tmp = tempfile::tempdir().unwrap();
        let workdir = setup_repo(tmp.path());
        let TestState { state, secrets, .. } =
            test_state(service_for(fake_script("claude", AGENT), None));
        // 没有可用的 GitHub 接口：连不上一个不存在的地址
        *state.gh_api.lock().unwrap() = "http://127.0.0.1:9".to_string();
        secrets.set("github_pat", "test-token").unwrap();
        let db = state.db.lock().unwrap();
        let id = tasks::create_manual(&db, "不会成功", "o/r", "随便改改。")
            .unwrap()
            .id;
        drop(db);
        let opts = StartOptions {
            adapter: "claude-cli".into(),
            workdir: workdir.to_string_lossy().into_owned(),
            base_branch: Some("main".into()),
            ..Default::default()
        };
        // 取不到默认分支（GitHub 接口连不上）：任务记为失败，原因留在任务上
        let task = start(
            &state,
            id,
            &StartOptions {
                base_branch: None,
                ..opts.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(task.status, "failed", "{task:?}");
        assert!(
            task.failure.as_deref().unwrap().contains("网络错误"),
            "{}",
            task.failure.as_deref().unwrap_or("")
        );
        assert!(task.worktree.is_none(), "还没建工作树就失败了");

        // 目录不是这个仓库
        let other = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&other).unwrap();
        let task = start(
            &state,
            id,
            &StartOptions {
                workdir: other.to_string_lossy().into_owned(),
                ..opts.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(task.status, "failed");
        assert!(
            task.failure.as_deref().unwrap().contains("不是 o/r")
                || task.failure.as_deref().unwrap().contains("git"),
            "{}",
            task.failure.as_deref().unwrap_or("")
        );

        // Agent 没改任何文件
        let talker = r#"printf '%s\n' '{"type":"result","is_error":false,"result":"我觉得不需要改。"}'; sleep 0.3"#;
        let idle = service_for(fake_script("claude", talker), None);
        let TestState {
            state: state2,
            secrets: secrets2,
            ..
        } = test_state(idle);
        secrets2.set("github_pat", "test-token").unwrap();
        let id2 = tasks::create_manual(&state2.db.lock().unwrap(), "不用改", "o/r", "看看就好。")
            .unwrap()
            .id;
        // 假 Agent 输出一行就退出：start 返回的时候这一轮可能已经收尾、任务已经不是「进行中」了，
        // 所以这里不断言它此刻的状态，只等它走到头
        start(&state2, id2, &opts).await.unwrap();
        let task = wait_status(&state2, id2, &["failed", "changes"]).await;
        assert_eq!(task.status, "failed");
        assert!(
            task.failure
                .as_deref()
                .unwrap()
                .contains("没有改动任何文件"),
            "{}",
            task.failure.as_deref().unwrap_or("")
        );
    }
}

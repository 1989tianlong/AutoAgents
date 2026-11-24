import { invoke } from "@tauri-apps/api/core";
import type {
  AgentEntry,
  AppEvent,
  AppInfo,
  ChatMessage,
  ChatSession,
  DiffView,
  DiscoveredAgent,
  EventCounts,
  GhIssue,
  GhPr,
  GhRepo,
  GhUser,
  InstallCommand,
  LiveTurn,
  LsResult,
  MachineView,
  Metrics,
  ModelList,
  RelayStatusView,
  RelayView,
  Task,
  UsageSummary,
} from "./types";

export const getAppInfo = () => invoke<AppInfo>("app_info");

/** 扫描本机 PATH 与 /Applications，发现已安装的 Agent */
export const discoverAgents = () => invoke<DiscoveredAgent[]>("discover_agents");

/** 所有机器上的 Agent：本机实时扫描，远程机器用最近一次同步到的清单 */
export const agentsAll = () => invoke<AgentEntry[]>("agents_all");

// ---------- 模型 ----------

/** 某个 Agent 在指定机器上可选的模型（读那台机器上的配置文件 / CLI） */
export const agentModels = (adapter: string, nodeId?: string | null) =>
  invoke<ModelList>("agent_models", { adapter, nodeId });

/** 记住手动填写的模型 */
export const modelRemember = (adapter: string, model: string) =>
  invoke<void>("model_remember", { adapter, model });

// ---------- GitHub ----------

export const githubConnect = (token: string) => invoke<GhUser>("github_connect", { token });
export const githubStatus = () => invoke<GhUser | null>("github_status");
export const githubDisconnect = () => invoke<void>("github_disconnect");
export const githubRepos = () => invoke<GhRepo[]>("github_repos");
export const githubIssues = (owner: string, repo: string) =>
  invoke<GhIssue[]>("github_issues", { owner, repo });
export const githubPrs = (owner: string, repo: string) =>
  invoke<GhPr[]>("github_prs", { owner, repo });

// ---------- 任务 ----------

export const tasksList = () => invoke<Task[]>("tasks_list");
export const taskCreateIssue = (p: {
  title: string;
  repo: string;
  issueNumber: number;
  issueUrl: string;
}) => invoke<Task>("task_create_issue", p);
export const taskSetStatus = (id: number, status: string) =>
  invoke<void>("task_set_status", { id, status });
export const taskDelete = (id: number) => invoke<void>("task_delete", { id });
export const taskGet = (id: number) => invoke<Task>("task_get", { id });

/** 手动建一个任务：一段需求说明，不经过 Issue */
export const taskCreateManual = (p: { title: string; repo: string; instructions: string }) =>
  invoke<Task>("task_create_manual", p);

/** 启动任务：在那台机器上准备工作树，把需求交给 Agent */
export const taskStart = (p: {
  id: number;
  nodeId: string;
  adapter: string;
  model: string | null;
  workdir: string;
  baseBranch: string | null;
  autoPr: boolean;
}) => invoke<Task>("task_start", p);

/** 人工确认改动之后：提交、推送、开 PR */
export const taskSubmit = (id: number) => invoke<Task>("task_submit", { id });

/** PR 的检查没通过：把失败的日志交给 Agent 修 */
export const taskFix = (id: number) => invoke<void>("task_fix", { id });
export const taskRefresh = (id: number) => invoke<Task>("task_refresh", { id });
export const tasksRefresh = () => invoke<number>("tasks_refresh");

/** 停止任务正在跑的这一轮 */
export const taskAbort = (id: number) => invoke<void>("task_abort", { id });

/** 任务工作树里的改动 */
export const taskDiff = (id: number) => invoke<DiffView>("task_diff", { id });
export const taskDiffFile = (id: number, path: string, untracked: boolean) =>
  invoke<string>("task_diff_file", { id, path, untracked });

/** 把仓库克隆到那台机器上 */
export const repoClone = (nodeId: string, url: string, dir: string) =>
  invoke<string>("repo_clone", { nodeId, url, dir });

// ---------- 会话 ----------

export const chatSessions = () => invoke<ChatSession[]>("chat_sessions");
export const chatMessages = (sessionId: number) =>
  invoke<ChatMessage[]>("chat_messages", { sessionId });

/** 开启新会话并发送第一条消息；model / workdir 留空时沿用 Agent 默认值 / 那台机器的主目录。
 *  nodeId 留空表示在本机执行。 */
export const chatStart = (
  adapter: string,
  model: string | null,
  workdir: string | null,
  prompt: string,
  nodeId?: string | null,
) => invoke<number>("chat_start", { adapter, model, workdir, prompt, nodeId });

export const chatSend = (sessionId: number, prompt: string) =>
  invoke<void>("chat_send", { sessionId, prompt });

/** 停止正在运行的一轮，已产出的文本会保留 */
export const chatStop = (sessionId: number) => invoke<void>("chat_stop", { sessionId });

/** 正在运行的会话及其实时输出：界面以它为准，补上没送达的事件 */
export const chatLive = () => invoke<LiveTurn[]>("chat_live");

// ---------- 机器 ----------

export const machinesList = () => invoke<MachineView[]>("machines_list");
export const nodesList = () => invoke<MachineView[]>("nodes_list");

/** 某台机器此刻的资源占用 */
export const nodeMetrics = (nodeId: string) => invoke<Metrics>("node_metrics", { nodeId });

/** 列出某台机器上的子目录（远程选择工作目录用） */
export const nodeLs = (nodeId: string, path: string) =>
  invoke<LsResult>("node_ls", { nodeId, path });

/** 在某台机器上的指定目录执行 git */
export const nodeGit = (nodeId: string, workdir: string, args: string[]) =>
  invoke<{ code: number | null; stdout: string; stderr: string }>("node_git", {
    nodeId,
    workdir,
    args,
  });

export const relayGet = () => invoke<RelayView>("relay_get");
export const relayClear = () => invoke<void>("relay_clear");
export const relayStatus = () => invoke<RelayStatusView>("relay_status");

/** 设置中继：粘贴中继启动时打印的连接串即可；也可以分开填地址、令牌、指纹 */
export const relaySet = (p: {
  text?: string | null;
  addr?: string | null;
  token?: string | null;
  fingerprint?: string | null;
}) => invoke<RelayView>("relay_set", p);

export const installScriptSet = (url: string) => invoke<void>("install_script_set", { url });

/** 给新机器生成安装命令（带一次性配对码，10 分钟内有效） */
export const nodeInstallCommand = (name: string, allow: string[]) =>
  invoke<InstallCommand>("node_install_command", { name, allow });

/** 与中继上一台在线、尚未配对的机器配对 */
export const nodePair = (nodeId: string, code: string, name?: string | null) =>
  invoke<MachineView>("node_pair", { nodeId, code, name });

/** 直连配对：粘贴那台机器上 `agentd pair` 打印的连接串 */
export const nodePairDirect = (text: string) => invoke<MachineView>("node_pair_direct", { text });

/** 让一台已配对的机器为另一台设备（另一台电脑上的 AutoAgents）开放配对 */
export const nodeAuthorize = (nodeId: string) => invoke<string>("node_authorize", { nodeId });

export const nodeRemove = (nodeId: string) => invoke<void>("node_remove", { nodeId });
export const nodeRename = (nodeId: string, name: string) =>
  invoke<MachineView>("node_rename", { nodeId, name });

/** 立刻重连某台机器 */
export const nodeConnectNow = (nodeId: string) => invoke<void>("node_connect_now", { nodeId });

// ---------- 活动与审计 ----------

export const eventsList = (limit = 100, kind?: string | null) =>
  invoke<AppEvent[]>("events_list", { limit, kind });
export const eventsCounts = () => invoke<EventCounts>("events_counts");
/** 清空事件；keep > 0 时保留最近 keep 条 */
export const eventsClear = (keep = 0) => invoke<number>("events_clear", { keep });

// ---------- 用量与成本 ----------

/** 用量汇总：总计、最近几天、按 Agent / 机器 / 会话（不同 Agent 口径不同，费用与 token 分开看） */
export const usageSummary = (days = 14) => invoke<UsageSummary>("usage_summary", { days });

import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  ChatMessage,
  ChatSession,
  DiscoveredAgent,
  GhIssue,
  GhPr,
  GhRepo,
  GhUser,
  LiveTurn,
  ModelList,
  Task,
} from "./types";

export const getAppInfo = () => invoke<AppInfo>("app_info");

/** 扫描本机 PATH 与 /Applications，发现已安装的 Agent */
export const discoverAgents = () => invoke<DiscoveredAgent[]>("discover_agents");

// ---------- 模型 ----------

/** 某个 Agent 在本机可选的模型（从其配置文件 / CLI 读取） */
export const agentModels = (adapter: string) =>
  invoke<ModelList>("agent_models", { adapter });

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
}) => invoke<number>("task_create_issue", p);
export const taskSetStatus = (id: number, status: string) =>
  invoke<void>("task_set_status", { id, status });
export const taskDelete = (id: number) => invoke<void>("task_delete", { id });

// ---------- 会话 ----------

export const chatSessions = () => invoke<ChatSession[]>("chat_sessions");
export const chatMessages = (sessionId: number) =>
  invoke<ChatMessage[]>("chat_messages", { sessionId });

/** 开启新会话并发送第一条消息；model / workdir 留空时沿用 Agent 默认值 / 主目录 */
export const chatStart = (
  adapter: string,
  model: string | null,
  workdir: string | null,
  prompt: string,
) => invoke<number>("chat_start", { adapter, model, workdir, prompt });

export const chatSend = (sessionId: number, prompt: string) =>
  invoke<void>("chat_send", { sessionId, prompt });

/** 停止正在运行的一轮，已产出的文本会保留 */
export const chatStop = (sessionId: number) => invoke<void>("chat_stop", { sessionId });

/** 正在运行的会话及其实时输出：界面以它为准，补上没送达的事件 */
export const chatLive = () => invoke<LiveTurn[]>("chat_live");

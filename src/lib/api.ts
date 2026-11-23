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
  Task,
} from "./types";

export const getAppInfo = () => invoke<AppInfo>("app_info");

/** 扫描本机 PATH 与 /Applications，发现已安装的 Agent */
export const discoverAgents = () => invoke<DiscoveredAgent[]>("discover_agents");

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
export const chatStart = (adapter: string, model: string | null, prompt: string) =>
  invoke<number>("chat_start", { adapter, model, prompt });
export const chatSend = (sessionId: number, prompt: string) =>
  invoke<void>("chat_send", { sessionId, prompt });

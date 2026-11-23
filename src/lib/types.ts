export type AgentKind = "cli" | "desktop";
export type AdapterLevel = "L1" | "L2" | "L3";

/** 本机发现的一个 Agent（对应 Rust 端 discovery::DiscoveredAgent） */
export interface DiscoveredAgent {
  id: string;
  name: string;
  kind: AgentKind;
  level: AdapterLevel;
  adapter: string;
  path: string;
  version: string | null;
}

export interface AppInfo {
  name: string;
  version: string;
  os: string;
  arch: string;
  hostname: string;
}

// ---------- GitHub ----------

export interface GhUser {
  login: string;
  name: string | null;
  avatarUrl: string | null;
}

export interface GhRepo {
  id: number;
  fullName: string;
  private: boolean;
  description: string | null;
  openIssues: number;
  updatedAt: string | null;
  htmlUrl: string;
}

export interface GhIssue {
  number: number;
  title: string;
  state: string;
  author: string;
  labels: string[];
  updatedAt: string | null;
  htmlUrl: string;
}

export interface GhPr {
  number: number;
  title: string;
  state: string;
  draft: boolean;
  author: string;
  headRef: string;
  baseRef: string;
  updatedAt: string | null;
  htmlUrl: string;
}

// ---------- 任务 ----------

export type TaskStatus = "pending" | "running" | "reviewing" | "done" | "failed";

export interface Task {
  id: number;
  title: string;
  source: string;
  repo: string | null;
  issueNumber: number | null;
  issueUrl: string | null;
  agentId: string | null;
  status: TaskStatus;
  branch: string | null;
  createdAt: string;
  updatedAt: string;
}

// ---------- 会话 ----------

export interface ChatSession {
  id: number;
  adapter: string;
  title: string;
  createdAt: string;
}

export interface ChatMessage {
  id: number;
  sessionId: number;
  role: "user" | "assistant" | "system";
  content: string;
  costUsd: number | null;
  createdAt: string;
}

export type ChatEvent =
  | { type: "delta"; sessionId: number; text: string }
  | { type: "done"; sessionId: number; costUsd: number | null }
  | { type: "error"; sessionId: number; message: string };

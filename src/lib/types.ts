export type AgentKind = "cli" | "desktop";
export type AdapterLevel = "L1" | "L2" | "L3";

/** 已实现托管会话的 CLI Agent（与 Rust 端 adapters.rs 保持一致） */
export const CHAT_ADAPTERS = ["claude-cli", "codex-cli", "grok-cli", "pi"] as const;

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
  home: string;
}

// ---------- 模型 ----------

export interface ModelOption {
  /** 传给 CLI 的模型标识 */
  value: string;
  label: string;
  /** 补充说明（来源渠道等） */
  detail: string | null;
}

export interface ModelList {
  adapter: string;
  /** 模型清单的来源（本机配置文件或命令） */
  source: string;
  /** 不选模型时 CLI 实际使用的默认值 */
  default: string | null;
  options: ModelOption[];
  /** 用户手动填写过的模型 */
  custom: string[];
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
  model: string | null;
  workdir: string | null;
  createdAt: string;
}

export interface ChatMessage {
  id: number;
  sessionId: number;
  role: "user" | "assistant" | "system";
  content: string;
  costUsd: number | null;
  inputTokens: number | null;
  outputTokens: number | null;
  createdAt: string;
}

/** 正在运行的一轮对话目前的输出（以 Rust 端为准的状态快照） */
export interface LiveTurn {
  sessionId: number;
  text: string;
  activity: string | null;
}

export type ChatEvent =
  | { type: "delta"; sessionId: number; text: string }
  /** 过程提示：工具调用、思考等 */
  | { type: "notice"; sessionId: number; text: string }
  /** 本轮结束时的完整文本，用它校正流式预览 */
  | { type: "replace"; sessionId: number; text: string }
  | {
      type: "done";
      sessionId: number;
      costUsd: number | null;
      inputTokens: number | null;
      outputTokens: number | null;
    }
  | { type: "error"; sessionId: number; message: string };

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
  /** 合并 PR 的目标分支；任务没指定基础分支时以它为准 */
  defaultBranch: string;
  /** `git@github.com:owner/repo.git`：在机器上克隆时用它 */
  sshUrl: string;
}

export interface GhIssue {
  number: number;
  title: string;
  /** 正文（Markdown） */
  body: string | null;
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

/** 任务的流转：待启动 → 进行中 →（待确认 →）审查中 → 已完成；任何一步都可能失败 */
export type TaskStatus = "pending" | "running" | "changes" | "reviewing" | "done" | "failed";

/** PR 上的一项检查（CI 的一个 job，或别人上报的一个状态） */
export interface GhCheck {
  name: string;
  state: "pending" | "success" | "failure" | "skipped";
  url: string | null;
}

export interface Task {
  id: number;
  title: string;
  /** `issue`：从 Issue 转来的；`manual`：直接写的一段需求 */
  source: "issue" | "manual";
  repo: string | null;
  issueNumber: number | null;
  issueUrl: string | null;
  agentId: string | null;
  status: TaskStatus;
  branch: string | null;
  createdAt: string;
  updatedAt: string;
  /** 在哪台机器上做 */
  nodeId: string | null;
  adapter: string | null;
  model: string | null;
  /** 仓库在那台机器上的位置 */
  workdir: string | null;
  /** 这个任务专用的工作树 */
  worktree: string | null;
  baseBranch: string | null;
  baseSha: string | null;
  /** 最近一次推送的提交 */
  headSha: string | null;
  /** Agent 的全部往来都在这个会话里 */
  sessionId: number | null;
  prNumber: number | null;
  prUrl: string | null;
  /** 最近一步的说明 */
  detail: string | null;
  /** 失败原因 */
  failure: string | null;
  /** 手动任务的需求说明 */
  instructions: string | null;
  /** Agent 做完后不等确认，直接提交并开 PR */
  autoPr: boolean;
  checks: GhCheck[] | null;
}

export interface DiffEntry {
  path: string;
  /** `M` 修改、`A` 新增、`D` 删除、`R` 重命名、`U` 冲突、`?` 未跟踪 */
  status: string;
}

/** 任务的工作树里相对起点提交的改动 */
export interface DiffView {
  worktree: string;
  branch: string | null;
  baseBranch: string | null;
  /** 比起点多出来的提交数 */
  ahead: number;
  stat: string;
  entries: DiffEntry[];
}

// ---------- 会话 ----------

export interface ChatSession {
  id: number;
  adapter: string;
  title: string;
  model: string | null;
  workdir: string | null;
  /** 会话所在机器的节点 id（`local` 表示本机） */
  nodeId: string;
  /** 这个会话属于哪个任务（流水线开的会话才有） */
  taskId: number | null;
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
  /** 这一轮从发出到结束的耗时 */
  durationMs: number | null;
  createdAt: string;
}

/** 正在运行的一轮对话目前的输出（以 Rust 端为准的状态快照） */
export interface LiveTurn {
  sessionId: number;
  text: string;
  activity: string | null;
  /** 在哪台机器上运行 */
  nodeId: string;
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

// ---------- 机器 ----------

/** 一台机器的静态信息（对应 Rust 端 autoagents_core::machine::MachineInfo） */
export interface MachineInfo {
  name: string;
  hostname: string;
  os: string;
  arch: string;
  kernel: string | null;
  /** 提供机器侧服务的程序版本：桌面应用内置，或 agentd */
  version: string;
  flavor: string;
  home: string;
  uptimeS: number;
  cpuCount: number;
  memTotal: number;
}

/** 一台机器此刻的资源占用 */
export interface Metrics {
  cpuPercent: number;
  memUsed: number;
  memTotal: number;
  load1: number;
  load5: number;
  load15: number;
  uptimeS: number;
  /** 这台机器上正在运行的对话轮数 */
  runningTurns: number;
}

export interface NodeStatus {
  online: boolean;
  /** 最近一次连不上或断开的原因 */
  error: string | null;
  /** 这次连上的时刻（Unix 毫秒） */
  sinceMs: number | null;
  lastSeenMs: number | null;
  /** 保活往返耗时 */
  rttMs: number | null;
}

/** 机器页的一行：本机永远是第一行 */
export interface MachineView {
  id: string;
  name: string;
  local: boolean;
  online: boolean;
  /** 远程机器至少连上过一次才有 */
  info: MachineInfo | null;
  status: NodeStatus | null;
  agentCount: number;
}

export interface RelayView {
  configured: boolean;
  addr: string;
  fingerprint: string;
  hasToken: boolean;
  /** 给新机器用的安装脚本地址 */
  installScript: string;
}

/** 中继上一台在线的机器 */
export interface OnlineNode {
  nodeId: string;
  name: string;
  os: string;
  sinceMs: number;
  paired: boolean;
}

/** 一份还没用掉的接入邀请：安装命令已经发给某台机器 */
export interface InviteView {
  name: string;
  /** 剩余有效期（秒） */
  expiresInS: number;
}

export interface RelayStatusView {
  relay: RelayView;
  connected: boolean;
  message: string | null;
  online: OnlineNode[];
  pending: InviteView[];
}

/** Rust 端推来的机器状态变化 */
export interface NodeStatusEvent {
  id: string;
  name: string;
  online: boolean;
  message: string | null;
}

export interface InstallCommand {
  /** 机器名字 */
  name: string;
  /** 一次性配对码 */
  code: string;
  command: string;
  expiresInS: number;
}

/** 某台机器上的子目录列表（选择工作目录用） */
export interface LsResult {
  path: string;
  parent: string | null;
  entries: { name: string; git: boolean }[];
}

/** 所有机器上的一个 Agent */
export interface AgentEntry extends DiscoveredAgent {
  nodeId: string;
  nodeName: string;
  online: boolean;
}

// ---------- 活动与审计 ----------

export interface AppEvent {
  id: number;
  ts: string;
  /** `node.online` / `session.started` / `task.status` / `deploy.state` … */
  kind: string;
  level: "info" | "warning" | "error";
  source: string;
  message: string;
  /** 界面上可以点过去的位置（如 `#/sessions?s=12`） */
  link: string | null;
}

export interface EventCounts {
  total: number;
  warning: number;
  error: number;
  /** 各个模块（kind 的第一段）的条数 */
  byKind: Record<string, number>;
}

// ---------- 用量与成本 ----------

export interface UsageBucket {
  name: string;
  turns: number;
  costUsd: number;
  inputTokens: number;
  outputTokens: number;
}

export interface UsageSummary {
  turns: number;
  /** 有费用口径的轮次合计 */
  costUsd: number;
  inputTokens: number;
  outputTokens: number;
  /** 平均每轮耗时（毫秒） */
  avgDurationMs: number;
  last24hCostUsd: number;
  last24hTurns: number;
  byDay: { day: string; turns: number; costUsd: number; tokens: number }[];
  byAgent: UsageBucket[];
  byMachine: UsageBucket[];
  topSessions: UsageBucket[];
}

// ---------- 部署 ----------

/** 部署目标属于哪个平台 */
export type DeployProvider = "vercel" | "cloudflare-pages" | "cloudflare-workers";

/** 登记了的部署目标：Vercel 项目、Cloudflare Pages 项目或 Worker */
export interface DeployTarget {
  id: number;
  provider: DeployProvider;
  name: string;
  /** 平台上的标识：Vercel 的项目 id、Worker 的标签；Pages 项目就是名字 */
  platformId: string;
  /** 属于哪个团队 / 账户；空表示 Vercel 个人账户 */
  scope: string;
  /** 生产地址 */
  url: string | null;
  /** 关联的 GitHub 仓库 */
  repo: string | null;
  repoId: string | null;
  /** 平台那边连的 Git 服务（github …）；空表示没连 */
  gitType: string | null;
  /** 生产分支 */
  branch: string | null;
  framework: string | null;
  /** 应用里发起生产部署要先确认 */
  prodRequiresApproval: boolean;
  /** 生产部署出问题时自动建修复任务 */
  autoFix: boolean;
  healthPath: string;
  healthContains: string | null;
  /** Workers：存了 Deploy Hook 的地址 */
  hasHook: boolean;
  createdAt: string;
  updatedAt: string;
}

export type DeployState = "queued" | "building" | "ready" | "error" | "canceled" | "other";

/** 一次部署：三个平台拉回来后归成同一种样子 */
export interface Deployment {
  id: string;
  targetId: number;
  provider: DeployProvider;
  project: string;
  env: "production" | "preview";
  state: DeployState;
  rawState: string | null;
  url: string | null;
  /** 平台控制台里这一次部署的页面 */
  inspectorUrl: string | null;
  branch: string | null;
  commitSha: string | null;
  commitMessage: string | null;
  author: string | null;
  createdMs: number | null;
  buildingMs: number | null;
  readyMs: number | null;
  errorCode: string | null;
  errorMessage: string | null;
  /** 能回滚到这一次 */
  rollbackOk: boolean;
  /** 现在线上跑的就是这一次 */
  current: boolean;
  guarded: boolean;
  /** 健康检查结果：ok，或没通过 / 没法检查的原因 */
  health: string | null;
  /** 守护为这次失败建的修复任务 */
  taskId: number | null;
}

/** 平台上还没登记的项目 */
export interface DeployCandidate {
  provider: DeployProvider;
  name: string;
  platformId: string;
  url: string | null;
  repo: string | null;
  repoId: string | null;
  branch: string | null;
  framework: string | null;
  registered: number | null;
}

export interface DeployScope {
  id: string;
  name: string;
}

export interface DeployProviderStatus {
  configured: boolean;
  /** 令牌验证没通过的原因；通过时为空 */
  error: string | null;
  who: string | null;
  /** Vercel 的团队 / Cloudflare 的账户 */
  scopes: DeployScope[];
  /** 选中的那一个 */
  scope: string | null;
}

export interface DeployStatus {
  vercel: DeployProviderStatus;
  cloudflare: DeployProviderStatus;
}

/** 可以回滚到的一次部署 */
export interface DeployHistoryItem {
  id: string;
  createdMs: number | null;
  branch: string | null;
  commitMessage: string | null;
  current: boolean;
  /** Workers：这一次部署用的版本 */
  versionId: string | null;
}

import { invoke } from "@tauri-apps/api/core";
import type {
  AgentEntry,
  AppEvent,
  AppInfo,
  ChatMessage,
  ChatSession,
  DeployCandidate,
  DeployHistoryItem,
  DeployProviderStatus,
  DeployStatus,
  DeployTarget,
  Deployment,
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
  Workflow,
  WorkflowTemplate,
  AutomationRule,
  AutomationRun,
  Inbox,
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

// ---------- 部署 ----------

/** 两家平台的连接状态（会拿保存的令牌各验证一次） */
export const deployStatus = () => invoke<DeployStatus>("deploy_status");

/** 填入 API 令牌：验证通过才存进系统钥匙串 */
export const deployConnect = (platform: string, token: string) =>
  invoke<DeployProviderStatus>("deploy_connect", { platform, token });

export const deployDisconnect = (platform: string) => invoke<void>("deploy_disconnect", { platform });

/** 选定在哪个团队 / 账户下工作 */
export const deploySetScope = (platform: string, scope: string | null) =>
  invoke<void>("deploy_set_scope", { platform, scope });

/** 平台上还没登记的项目 */
export const deployDiscover = (platform: string) => invoke<DeployCandidate[]>("deploy_discover", { platform });

export const deployTargets = () => invoke<DeployTarget[]>("deploy_targets");

/** 登记一个部署目标。`url` 给了就当生产地址；Workers 要主动触发构建还得给 Deploy Hook 的地址 */
export const deployTargetAdd = (
  provider: string,
  name: string,
  platformId: string,
  url: string | null,
  hookUrl: string | null,
) => invoke<DeployTarget>("deploy_target_add", { provider, name, platformId, url, hookUrl });

/** 改目标设置：传了哪项改哪项 */
export const deployTargetUpdate = (
  id: number,
  patch: {
    url?: string | null;
    prodRequiresApproval?: boolean;
    autoFix?: boolean;
    autoPr?: boolean;
    healthPath?: string | null;
    healthContains?: string | null;
    hookUrl?: string | null;
  },
) => invoke<DeployTarget>("deploy_target_update", { id, ...patch });

/** 取消登记（平台上的项目不动） */
export const deployTargetRemove = (id: number) => invoke<void>("deploy_target_remove", { id });

/** 本地记着的部署，新的在前 */
export const deployList = (targetId: number | null, limit?: number) =>
  invoke<Deployment[]>("deploy_list", { targetId, limit: limit ?? null });

/** 现在就向平台拉一次部署状态 */
export const deployRefresh = (targetId: number | null) =>
  invoke<number>("deploy_refresh", { targetId });

/** 主动部署：Vercel 按分支（或提交）；Pages 把这条分支最近的一次重做；Workers 触发 Deploy Hook */
export const deployTrigger = (targetId: number, branch: string | null, commit: string | null, confirmed: boolean) =>
  invoke<Deployment>("deploy_trigger", { targetId, branch, commit, confirmed });

/** 把某一次部署原样再来一遍 */
export const deployRetry = (id: string, confirmed: boolean) =>
  invoke<Deployment>("deploy_retry", { id, confirmed });

export const deployCancel = (id: string) => invoke<void>("deploy_cancel", { id });

/** Vercel：把一次预览部署直接提升为生产 */
export const deployPromote = (id: string) => invoke<void>("deploy_promote", { id });

/** 生产流量切回之前的某一次部署；Workers 回滚的单位是版本 */
export const deployRollback = (targetId: number, toId: string, versionId: string | null) =>
  invoke<void>("deploy_rollback", { targetId, toId, versionId });

export const deployHistory = (targetId: number) =>
  invoke<DeployHistoryItem[]>("deploy_history", { targetId });

/** 构建日志 */
export const deployLogs = (id: string) => invoke<string>("deploy_logs", { id });

/** 现在就查一次健康，返回结果 */
export const deployCheckHealth = (id: string) => invoke<string>("deploy_check_health", { id });

/** 这个任务的分支（或它推上去的提交）对应的部署 */
export const deployForTask = (taskId: number) => invoke<Deployment[]>("deploy_for_task", { taskId });

// ---------- 工作流编排 ----------

/** 内置的多 Agent 流程模板 */
export const workflowTemplates = () => invoke<WorkflowTemplate[]>("workflow_templates");

export const workflowList = () => invoke<Workflow[]>("workflow_list");
export const workflowGet = (id: number) => invoke<Workflow>("workflow_get", { id });

/** 从模板建一个工作流（还没启动）。Issue 的标题与地址界面上带上，缺了后端会去拉 */
export const workflowCreate = (p: {
  template: string;
  repo: string;
  issueNumber?: number | null;
  issueTitle?: string | null;
  issueUrl?: string | null;
  prNumber?: number | null;
  instructions?: string | null;
  nodeId: string;
  workdir: string;
  model?: string | null;
  autoPr?: boolean;
  /** 按步骤顺序覆盖每一步用哪个 Agent */
  adapters?: string[];
}) => invoke<Workflow>("workflow_create", p);

export const workflowStart = (id: number) => invoke<Workflow>("workflow_start", { id });
export const workflowCancel = (id: number) => invoke<Workflow>("workflow_cancel", { id });

/** 人工评审：在候选方案（依赖的任务）里选定一个 */
export const workflowGate = (workflowId: number, stepId: number, chosenTaskId: number) =>
  invoke<Workflow>("workflow_gate", { workflowId, stepId, chosenTaskId });

/** 重跑失败的工作流：失败的与被跳过的步骤重新排队 */
export const workflowRetry = (id: number) => invoke<Workflow>("workflow_retry", { id });

// ---------- 自动化规则 ----------

/** 已建的自动化规则（含近 24 小时触发次数） */
export const automationRules = () => invoke<AutomationRule[]>("automation_rules");

/** 规则的触发记录 */
export const automationRuns = (limit?: number) => invoke<AutomationRun[]>("automation_runs", { limit });

export const automationRuleCreate = (p: {
  name: string;
  kind: string;
  repo: string;
  label?: string | null;
  template?: string | null;
  adapters?: string[];
  model?: string | null;
  autoPr?: boolean;
  comment?: boolean;
  nodeId?: string | null;
  workdir?: string | null;
}) => invoke<AutomationRule>("automation_rule_create", p);

export const automationRuleSetEnabled = (id: number, enabled: boolean) =>
  invoke<AutomationRule>("automation_rule_set_enabled", { id, enabled });

export const automationRuleDelete = (id: number) => invoke<void>("automation_rule_delete", { id });

/** 立刻检查一遍规则（平时每两分钟一次） */
export const automationScan = () => invoke<number>("automation_scan");

// ---------- 决策中心 ----------

/** 等人决策的事：评审门、改动待确认、待启动、PR 等验收 */
export const inboxList = () => invoke<Inbox>("inbox_list");

/** 合并任务的 PR：验收的最后一步，合并后任务自动标完成 */
export const inboxMergePr = (taskId: number) => invoke<unknown>("inbox_merge_pr", { taskId });

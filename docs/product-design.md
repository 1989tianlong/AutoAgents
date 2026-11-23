# AutoAgents 产品功能设计

> 版本：v0.2 草案 · 2026-11
> 技术基线：Rust + Tauri 2 · React + TypeScript + Tailwind CSS
> 许可证：GPL-3.0（GNU General Public License v3.0）

---

## 1. 产品定位

**AutoAgents 是一个跨机器、多 Agent 的统一管理与协同工作台（桌面应用）。**

一句话：把你散落在**工作电脑、家庭电脑、服务器**上的所有 AI Agent（CLI 类与桌面类）纳管到一个应用里，远程监控与驱动它们，让它们**协同处理 GitHub 仓库中的 code / issues / pull requests**，并**自动发布到 Vercel / Cloudflare**。

### 解决的三个核心痛点

| 痛点 | 现状 | AutoAgents 方案 |
|---|---|---|
| Agent 散落各处 | 每台机器各自安装、各自开终端，状态不可见 | 统一面板：所有机器上的 Agent 状态一目了然 |
| 无法远程驱动 | 人在公司，家里的 Agent 用不上 | 节点 Daemon + 加密通道，随时随地调度任意机器上的 Agent |
| 多 Agent 无协同 | 单打独斗，产出需要人工搬运 | 工作流编排 + 以 GitHub 为协作介质，多 Agent 流水线作业 |

### Non-goals（明确不做）

- 不开发 Agent 本身，只做**管理与协同层**（Adapter 适配已有 Agent）
- 不做云端 SaaS 多租户，定位**个人/小团队自托管**
- 不做 Agent 间通用聊天室（协同以任务为载体，不做社交化功能）

---

## 2. 核心概念模型

```
Machine（机器节点）
 └── Agent（Agent 实例，经由 Adapter 接入）
      └── Session（一次交互会话）

Task（任务）──分配给──▶ Agent
Workflow（工作流）──编排──▶ 多个 Task / Agent
GitRepo / Issue / PR ◀──协作介质──▶ Agent 间异步通信
```

| 概念 | 说明 |
|---|---|
| **Machine** | 一台物理/虚拟机：工作电脑、家庭电脑、服务器。运行轻量 daemon（`agentd`） |
| **Agent** | 某机器上的一个 Agent 实例，如「服务器上的 codex」。有类型（cli/desktop）、能力集、运行状态 |
| **Adapter** | 针对每种 Agent 的适配器，声明其能力等级（见 §3），封装启动/对话/停止 |
| **Task** | 工作单元，如「修复 issue #12」。有状态机：pending → running → reviewing → done/failed |
| **Workflow** | 多 Agent 编排（DAG）：如 `claude 实现 → codex 审查 → grok 总结` |
| **Session** | 与某个 Agent 的一次交互上下文（消息流、token 用量、产物） |
| **Workspace** | 任务绑定的仓库本地 clone（分支隔离，任务结束可回收） |

---

## 3. Agent 接入分层（Adapter 能力等级）

不同 Agent 可被驱动的深度不同，Adapter 按能力分级，UI 据此展示可执行的操作：

| 等级 | 能力 | 覆盖 Agent（示例） | 驱动方式 |
|---|---|---|---|
| **L3 API 集成** | 双向流式对话、结构化输出、usage 上报 | claude cli（`-p --output-format stream-json` 双向流）、codex（`codex exec` + JSON）、pi、grok cli（headless） | spawn 子进程 + stdin/stdout JSON 流 |
| **L2 命令驱动** | 单发指令、拿回结果（非交互式） | 各 CLI 的 exec/print 模式 | 一次性子进程调用 |
| **L1 进程管理** | 启动/停止/重启、存活检测、资源占用 | 桌面 Agent：zcode、kimi code、grok bot、codex 桌面版 | 进程管理 + 窗口检测；macOS 可选 Accessibility 深度控制（后置） |

内置 Adapter 清单（首期）：

- **CLI**：`claude-cli`、`codex-cli`、`grok-cli`、`pi`
- **桌面**：`zcode`、`kimi-code`、`grok-bot`、`codex-desktop`
- **扩展机制**：Adapter 以 Rust trait + TOML 描述文件实现，支持用户自定义新增（声明二进制路径、启动参数模板、能力等级）

---

## 4. 系统架构

```
┌─────────────────────────────────────────────────┐
│              AutoAgents 桌面应用                 │
│  ┌───────────────────────────────────────────┐  │
│  │  前端 React + Tailwind（WebView）          │  │
│  └───────────────▲───────────────────────────┘  │
│                  │ Tauri IPC (invoke / event)    │
│  ┌───────────────┴───────────────────────────┐  │
│  │  Rust 核心（Hub）                          │  │
│  │  · 任务调度器   · 工作流引擎   · 事件总线   │  │
│  │  · GitHub/Vercel/CF 连接器 · SQLite · 密钥环│  │
│  └───────┬───────────────────┬───────────────┘  │
└──────────┼───────────────────┼──────────────────┘
           │ WSS (mDNS 局域网直连 │ 或 中继/Tailscale)
   ┌───────▼──────┐     ┌───────▼──────┐
   │ agentd(工作机)│     │ agentd(服务器)│  …… agentd(家庭机)
   │ · Agent 发现  │     │              │
   │ · 进程驱动    │     │              │
   │ · 心跳/指标   │     │              │
   └──────────────┘     └──────────────┘
```

要点：

1. **Hub-Spoke 拓扑**：桌面应用即控制中枢；每台机器部署一个 Rust 单二进制 daemon（`agentd`，~10MB，常驻内存 <20MB），负责本机 Agent 的发现与驱动。
2. **通信**：控制端 ↔ agentd 走 WebSocket + TLS，配对 token 认证；局域网 mDNS 自动发现，跨网经由 Tailscale/WireGuard 或自建中继（复用 GitHub 仓库做引导亦可）。
3. **GitHub 双角色**：既是**操作对象**（读 issue/PR、推分支），也是 **Agent 间异步协作介质**（Agent A 在 issue 评论 → Agent B 监听并接续），天然跨机器、天然留痕。
4. **状态库**：本地 SQLite（任务、会话、事件、用量），不依赖外部数据库。
5. **密钥安全**：GitHub PAT、SSH key、各 Agent API key 存系统 Keychain；密钥只留在所在机器，daemon 仅转发指令不落盘明文。
6. **发布通道**：Vercel / Cloudflare 连接器负责部署触发与状态监听，构建失败自动转为 Agent 修复任务，形成「改码 → PR → 发布 → 守护」闭环（见 §5.8）。

---

## 5. 功能模块设计

### 5.1 仪表盘（Dashboard）

- 顶部指标卡：在线机器数 / Agent 总数（运行中）、进行中任务、待处理 Issue/PR、今日 token 消耗与成本估算
- 机器健康一览（CPU/内存迷你图，agentd 心跳延迟）
- 最近任务动态流（谁在做什么、成败状态）
- 快捷入口：「从 Issue 创建任务」「一键跑工作流」

### 5.2 机器管理（Machines）

- 机器卡片：名称、标签（work / home / server）、在线状态、系统信息、agentd 版本
- 接入向导：一条 `curl -fsSL … | sh` 安装命令 + 配对码，扫码/输码完成绑定
- 远程运维：重启 agentd、查看机器日志、资源监控曲线
- 标签与分组：按「工作 / 家庭 / 服务器」分组展示，支持自定义标签

### 5.3 Agent 管理（Agents）

- 列表/卡片双视图，按 **机器 × 类型（CLI/桌面）× 状态** 筛选
- 每个 Agent 展示：所属机器、Adapter 等级徽章（L1/L2/L3）、运行状态、版本、最近活跃
- 生命周期操作：启动 / 停止 / 重启（L1 起）；「打开会话」（L2/L3）
- **自动发现**：agentd 扫描 PATH 与常见安装位置（`~/.local/bin`、`/opt/homebrew/bin`…）识别已装 Agent，一键纳管
- Agent 详情页：配置（模型、API key 引用）、历史会话、用量统计

### 5.4 会话与终端（Sessions）

- **托管会话**：应用内 Chat UI 直接与 CLI Agent 对话（底层走 headless 双向流），无需开终端
- 消息级元数据：token 用量、耗时、调用的工具
- **终端透传**：可打开一个真实终端面板 attach 到该 Agent 进程（排查问题用）
- 会话历史可检索、可导出 Markdown

### 5.5 任务中心（Tasks）

- 看板视图：`待处理 / 进行中 / 待审查 / 完成 / 失败`
- 任务来源：手动创建、Issue 一键转任务、Webhook 自动生成、工作流节点
- 任务详情：目标描述、执行 Agent、绑定仓库/分支、实时输出流（终端式渲染）、产物（diff、PR 链接）
- 任务操作：暂停 / 取消 / 转派其他 Agent / 重试
- **人工卡点**：涉及 push / merge / 评论前可要求人工确认（默认对写操作开启）

### 5.6 工作流编排（Workflows）

- 可视化 DAG 编辑器（React Flow）：节点 = Agent + 指令模板，边 = 依赖
- 内置模板：
  - **修 Issue 流水线**：`claude-cli 分析修复` → `codex-cli 审查 diff` → `grok-cli 撰写变更说明并评论`
  - **PR 审查流水线**：新 PR → 双 Agent 交叉审查 → 汇总结论
  - **竞争模式**：多 Agent 各自解同一 Issue，人择优 merge
  - **发布守护流水线**：PR merge → 触发 Vercel/CF 部署 → 健康检查 → 失败则解析构建日志并自动创建修复任务回灌 Agent
- 触发器：手动 / Issue 打标签（如 `agent-todo`）/ 新 PR / 定时
- 变量系统：`{{issue.title}}`、`{{repo}}`、`{{branch}}` 注入指令模板
- 运行视图：DAG 上实时高亮执行进度，节点可点开看输出

### 5.7 GitHub 集成（GitHub）

- 认证：PAT 或 GitHub App（OAuth device flow）
- 仓库面板：issues / PRs 列表（含筛选、标签），详情页含评论线程
- **Issue → 任务**：选中 issue → 选择执行 Agent / 工作流 → 创建任务（自动建分支 `agent/issue-12`）
- **PR 生命周期**：Agent 产出的 PR 打 `autoagents` 标签；审查 Agent 的结论以 review comment 呈现
- Webhook 监听（可选自建轻量 receiver，或轮询模式）
- 安全边界：默认不允许 Agent 直接 merge；分支保护建议提示

### 5.8 发布与部署（Deploy & Release）

面向用户仓库中的 Web 项目（AutoAgents 本身是桌面应用，无需部署）。支持 **Vercel** 与 **Cloudflare（Pages / Workers）** 两大平台。

- **部署目标（Deploy Target）**：登记 Vercel 项目 / CF Pages 项目 / CF Worker，绑定仓库与环境策略；平台 token 存 Keychain
- **三种触发模式**：
  1. **Git 集成（推荐，零配置）**：平台已关联仓库，push/merge 即自动部署，AutoAgents 负责**监听部署状态**并联动任务
  2. **API 主动触发**：经 Vercel Deployments API / Cloudflare API v4 指定 commit/分支发起部署
  3. **CLI 驱动**：将 `wrangler` 作为内置 CLI Adapter，由指定机器上的 agentd 执行 `wrangler deploy`（L2 能力，纳入 Agent 统一管理）
- **环境策略**：PR 创建/更新 → Preview 部署自动执行；production 部署默认**人工卡点**（可按项目放行）
- **状态面板**：部署列表（项目、环境、commit、状态、耗时、URL）；PR 详情页直接展示 Preview URL
- **一键回滚**：回退到任意历史部署（Vercel / CF 原生支持，应用内直接操作）
- **守护闭环**：部署失败 / 健康检查不过 → 抓取构建日志与错误摘要 → 自动创建修复任务分配给指定 Agent → 修复 PR → 重新部署，直到绿灯或达到重试上限

### 5.9 协同机制（Collaboration）

三种协同模式，按需组合：

1. **编排模式（同步）**：工作流引擎按 DAG 调度，Agent 产出直接作为下游输入（内存传递）
2. **GitHub 介质模式（异步）**：Agent A 完成后在 issue/PR 留下结构化评论（如 `@agent:codex 请审查`），监听该仓库的 Agent B 接续处理 —— 跨机器最稳妥，全程留痕
3. **竞争模式**：N 个 Agent 并行解同一任务，控制端汇总对比，人选最优

### 5.10 可观测性（Activity & Analytics）

- 统一日志流：所有机器/Agent/任务事件（可按来源过滤，保留策略可配）
- 用量与成本：按 Agent / 机器 / 任务 / 时间维度统计 token 与估算费用
- 审计日志：谁在何时让哪个 Agent 对哪个仓库做了什么写操作

### 5.11 设置（Settings）

- 通用：语言、主题（亮/暗/跟随系统）、开机自启
- 连接：中继地址、Tailscale 检测、agentd 端口
- GitHub：账号、仓库授权范围、Webhook 配置
- 部署：Vercel / Cloudflare 账号与 token、环境放行策略、健康检查规则
- 安全：写操作确认策略、密钥管理（Keychain）、配对设备管理（撤销）
- Adapter：自定义 Agent 类型管理

---

## 6. 关键用户旅程

**旅程 A：远程处理 Issue（核心场景）**

1. 人在工作电脑上打开 AutoAgents，仪表盘显示家庭电脑在线、其上 `claude-cli` 空闲
2. GitHub 面板选中 issue #23「登录超时未处理」，点击「转任务」，选工作流「修 Issue 流水线」
3. 任务创建：家庭电脑 claude-cli 在 `agent/issue-23` 分支修复并推送，创建 PR（打 `autoagents` 标签）
4. 服务器上的 codex-cli 自动接续审查，PR 上输出 review 意见
5. grok-cli（工作电脑）总结变更，评论到 PR
6. 用户收到通知 → 人工确认 → merge → 触发 Vercel 生产部署（已过人工卡点）
7. AutoAgents 监听部署状态：构建失败则自动建修复任务回灌 Agent；健康检查通过 → 任务关闭，issue 自动关联关闭

**旅程 B：日常纳管**

- 新买服务器 → 机器页复制安装命令 → 服务器执行 → 输入配对码 → agentd 自动发现已装的 codex/pi → 一键纳管完成

**旅程 C：随手对话**

- Agents 页点「会话」→ 与服务器上的 grok-cli 直接聊天排查日志，全程无 SSH

---

## 7. 数据模型（核心表）

```sql
machines(id, name, group, os, agentd_version, last_heartbeat, pubkey, paired_at)
agents(id, machine_id, kind, adapter, name, binary_path, level, status, config_json)
tasks(id, title, source, issue_id, repo_id, agent_id, workflow_run_id,
      status, branch, result_json, created_at, finished_at)
workflows(id, name, dag_json, trigger_json, enabled)
workflow_runs(id, workflow_id, task_id, status, started_at)
sessions(id, agent_id, task_id, messages_json, usage_json)
events(id, ts, source_type, source_id, level, message)   -- 日志/审计统一流
repos(id, owner, name, auth_ref, webhook_secret_ref)
deploy_targets(id, repo_id, provider, project, env_policy_json, auth_ref)
deployments(id, target_id, task_id, commit_sha, env, status, url, log_ref, created_at, finished_at)
usage(agent_id, ts, prompt_tokens, completion_tokens, est_cost)
```

---

## 8. 技术栈与工程约定

| 层 | 选型 |
|---|---|
| 桌面框架 | **Tauri 2**（Rust core，多窗口、系统托盘、全局快捷键、自更新） |
| 后端核心 | Rust：tokio、serde、rusqlite/sqlx、octocrab（GitHub）、reqwest（Vercel REST / Cloudflare API v4）、async-tungstenite |
| daemon | 独立 Rust crate `agentd`：axum + WS server 端能力 + 子进程驱动（portable-pty） |
| 前端 | **React 18 + TypeScript + Vite**、**Tailwind CSS**、Zustand（状态）、TanStack Query/Router |
| 可视化 | React Flow（工作流 DAG）、轻量图表（用量曲线） |
| IPC | Tauri `invoke` + `event`（进度流推送） |
| 许可证 | **GPL-3.0**：应用全部源码（Rust 核心、agentd、前端）以 GPL-3.0 发布 |

性能目标：应用冷启动 < 1.5s；空闲常驻内存 < 150MB（WebView 计入）；agentd < 20MB。

---

## 9. 版本路线图

| 里程碑 | 范围 | 验收标志 |
|---|---|---|
| **M1 单机版** | 本机 Agent 发现 + 纳管；CLI Agent 托管会话（claude/codex/grok/pi）；GitHub PAT 接入，Issue→任务→PR 全流程；任务看板 | 在一台机器上走通旅程 A（单 Agent 版） |
| **M2 多机** | agentd + 配对 + mDNS/中继通信；机器监控；远程会话 | 工作电脑调度家庭电脑/服务器上的 Agent |
| **M3 协同** | 工作流 DAG 编辑器与运行时；GitHub 介质异步协同；竞争模式；Webhook 触发；部署状态监听（Vercel/CF）+ Preview 联动 + 失败自动建任务 | 三 Agent 流水线自动处理一个真实 Issue 并产出 PR，PR 预览部署联动 |
| **M4 深水区** | 桌面 Agent 深度控制（Accessibility）；API 主动部署、一键回滚与发布守护闭环；用量成本分析；自动化规则引擎；自更新 | 桌面 Agent 可被有限驱动；部署失败自动修复闭环跑通；月度成本报告 |

---

## 10. 风险与对策

| 风险 | 对策 |
|---|---|
| CLI Agent 接口频繁变动（如 claude/codex 参数变更） | Adapter 层隔离 + 版本探测；TOML 声明式配置可热改 |
| 跨网络连通（NAT、无公网 IP） | 局域网 mDS 直连优先；推荐 Tailscale；中继仅做信令+转发 |
| Agent 产出质量不可控直接写仓库 | 默认分支隔离 + 写操作人工卡点 + PR 审查双保险 |
| 各 Agent 用量计费口径不一 | 统一 usage 事件模型，成本为「估算」并标注口径 |
| 自动发布殃及生产环境 | Preview/Production 环境分离；production 默认人工卡点；一键回滚兜底 |

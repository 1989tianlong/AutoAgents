# AutoAgents 产品功能设计

> 版本：v0.2.0 · 2026-11（实现状态已对照代码更新）
> 技术基线：Rust + Tauri 2 · React + TypeScript + Tailwind CSS
> 许可证：GPL-3.0（GNU General Public License v3.0）

---

## 1. 产品定位

**AutoAgents 是一个跨机器、多 Agent 的统一管理与协同工作台（桌面应用）。**

一句话：把你散落在**工作电脑、家庭电脑、服务器**上的所有 AI Agent（CLI 类与桌面类）纳管到一个应用里，跨互联网监控与驱动它们，让它们**处理 GitHub 仓库中的 Issue 与 PR**，并**盯着发布到 Vercel / Cloudflare 的每一次部署**。

### 解决的三个核心痛点

| 痛点 | 现状 | AutoAgents 方案 |
|---|---|---|
| Agent 散落各处 | 每台机器各自安装、各自开终端，状态不可见 | 统一面板：所有机器上的 Agent 状态一目了然 |
| 无法远程驱动 | 人在公司，家里的 Agent 用不上 | 节点 daemon + 中继，被管理的机器只要能出站就能接入 |
| 做完没人盯 | PR 合并了，部署挂了，要等有人发现 | 任务流水线把改动送到 PR；部署守护盯着线上，出问题自动建修复任务 |

### Non-goals（明确不做）

- 不开发 Agent 本身，只做**管理与协同层**（Adapter 适配已有 Agent）
- 不做云端 SaaS 多租户，定位**个人/小团队自托管**；中继可以自己架
- 不做 Agent 间通用聊天室（协同以任务为载体）

### 实现状态（v0.2.0）

| 模块 | 状态 | 说明 |
|---|---|---|
| Agent 发现与会话 | 已实现 | 本机扫描；claude / codex / grok / pi 的托管会话；远程机器上的会话；模型清单 |
| 多机接入 | 已实现 | `agentd` + 自建中继 / 直连；一次性配对码；端到端加密；机器指标；安装脚本与发布流程 |
| GitHub | 已实现 | PAT 连接；浏览 Issue / PR；Issue 转任务 |
| 任务流水线 | 已实现 | 独立工作树 → 看 diff → 确认后提交 / 推送 / 开 PR → 跟踪检查 → 合并 |
| 部署与守护 | 已实现，**未用真实账号验证** | Vercel / Cloudflare Pages / Workers 的部署跟踪、日志、发起、重做、取消、升为生产、回滚、健康检查、自动建修复任务 |
| 可观测 | 部分 | 仪表盘、活动记录、近 24 小时花费；按机器 / Agent / 项目的成本分析未做 |
| 工作流编排 | 未开始 | 页面上只有模板预览 |
| 桌面 Agent 深度控制 | 未开始 | 只做发现与展示 |

---

## 2. 核心概念模型

```
Machine（机器节点，跑着 agentd）
 └── Agent（Agent 实例，经由 Adapter 接入）
      └── Session（一次交互会话）

Task（任务）──分配给──▶ 某台机器上的 Agent
                     └──在独立的 git 工作树里干活 ──▶ PR ──▶ 部署
DeployTarget（登记的项目）──每次──▶ Deployment
Deployment（生产部署失败 / 健康不过）──守护──▶ 修复任务（一种 Task）
```

| 概念 | 说明 |
|---|---|
| **Machine** | 一台物理/虚拟机：工作电脑、家庭电脑、服务器。运行 `agentd`；运行 AutoAgents 的那台由应用内置的同一套实现提供，不用另装 |
| **Agent** | 某机器上的一个 Agent 实例，如「服务器上的 codex」。有类型（cli / desktop）、能力等级 |
| **Adapter** | 针对每种 Agent 的适配器，声明其能力等级（见 §3），封装启动 / 对话 / 停止 |
| **Task** | 工作单元，如「修复 issue #12」。状态：`pending`（待启动）→ `running`（进行中）→ `changes`（待确认）→ `reviewing`（审查中）→ `done`；任何一步都可能 `failed` |
| **Session** | 与某个 Agent 的一次交互上下文（消息流、token 用量）；任务流水线为任务开的会话归属那个任务 |
| **DeployTarget** | 登记进来的一个平台项目：Vercel 项目、Cloudflare Pages 项目、Worker。带生产分支、生产地址、健康检查规则、守护开关 |
| **Deployment** | 平台上的一次部署，拉回来后归成同一种样子：`queued` / `building` / `ready` / `error` / `canceled` / `other` |
| **Workflow** | 多 Agent 编排（规划中，见 §5.6） |

---

## 3. Agent 接入分层（Adapter 能力等级）

不同 Agent 可被驱动的深度不同，Adapter 按能力分级，UI 据此展示可执行的操作：

| 等级 | 能力 | 已接入 | 驱动方式 |
|---|---|---|---|
| **L3 流式双向** | 流式对话、多轮续聊、usage 上报、随时停止 | `claude-cli`、`codex-cli`、`grok-cli`、`pi` | 子进程 + stdin / stdout JSON 流 |
| **L2 单发指令** | 单发指令、拿回结果 | 暂无单独实现 | 一次性子进程调用 |
| **L1 进程管理** | 发现、展示版本与路径 | 桌面 Agent：ZCode、Kimi Code、Grok Bot、Codex 桌面版等 | 扫描 `/Applications` 与常见安装位置；深度控制未做 |

本机由应用实时扫描，其它机器由 `agentd` 扫描后在连上时同步。

---

## 4. 系统架构

```
┌─────────────────────────────────────────────────┐
│              AutoAgents 桌面应用（控制端）          │
│  ┌───────────────────────────────────────────┐  │
│  │  React + Tailwind（WebView）               │  │
│  └───────────────▲───────────────────────────┘  │
│                  │ Tauri IPC (invoke / event)    │
│  ┌───────────────┴───────────────────────────┐  │
│  │  Rust 核心                                 │  │
│  │  · 会话 · 机器与中继 · 任务流水线 · 部署守护  │  │
│  │  · GitHub / Vercel / Cloudflare 连接器      │  │
│  │  · SQLite（状态）· 系统钥匙串（凭据）         │  │
│  └───────┬───────────────────────────────────┘  │
└──────────┼──────────────────────────────────────┘
           │ 只出站；TLS + 端到端加密
   ┌───────▼──────────┐                ┌───────────▼──────┐
   │ autoagents-relay │                │ agentd（直连）     │
   │ 只验令牌、转发密文  │                └──────────────────┘
   └───────▲──────────┘
           │ 只出站
   ┌───────┴────────┬─────────────────┬──────────────┐
   │ agentd 工作电脑 │  agentd 家庭电脑 │  agentd 服务器 │
   └────────────────┴─────────────────┴──────────────┘
```

要点：

1. **控制端与节点**：桌面应用是控制中枢；每台被管理的机器跑一个单二进制 daemon（`agentd`），负责本机 Agent 的发现、按指令执行对话、上报资源占用。运行应用的那台机器由应用内置的同一套机器侧实现提供，不用另装。
2. **通信**：控制端与 agentd 之间是 WebSocket + TLS（自签名证书，按指纹钉住），上面再叠一层端到端加密（X25519 协商、ChaCha20-Poly1305）。两条接入路径都只需要被管理的机器能出站：
   - **经中继**（推荐，穿透 NAT）：双方各自连到自建中继，中继只验令牌、转发密文
   - **直连**：被管理的机器有公网地址时，控制端直接连它
3. **配对**：一次性配对码，10 分钟有效；配对成功后双方各保存一把长期密钥，之后的连接端到端加密。移除机器即作废它的密钥。局域网 mDNS 自动发现没有做。
4. **GitHub 双角色**：既是**操作对象**（读 Issue / PR、推分支），也是后续多 Agent 协作的介质（规划中）。
5. **状态库**：本地 SQLite（`app_data_dir/autoagents.db`），不依赖外部数据库。
6. **密钥安全**：GitHub PAT、中继令牌、Vercel / Cloudflare 令牌、与各台机器配对的密钥，全部存系统钥匙串（服务名 `com.tianlong.autoagents`），不进数据库、不写日志、不回传界面。
7. **发布通道**：Vercel / Cloudflare 连接器负责部署跟踪与守护，构建失败自动变成修复任务，形成「改码 → PR → 发布 → 守护」闭环（见 §5.8）。

---

## 5. 功能模块设计

标注「已实现」的与 v0.2.0 的代码一致；其余是规划。

### 5.1 仪表盘（Dashboard）— 已实现

- 指标卡：在线机器、进行中的对话、近 24 小时花费、GitHub 连接状态
- 近几天每天的花费 / token 柱状图；最近的活动记录
- 快捷入口：与 Agent 对话、接入一台机器、从 Issue 创建任务、配置部署目标

规划中：机器健康一览（CPU / 内存迷你图）、按机器 / Agent 的成本曲线。

### 5.2 机器管理（Machines）— 已实现

- 本机与所有配对过的机器：在线状态、延迟、上次在线时间、系统信息、CPU / 内存、运行时长、agentd 版本、Agent 数量
- **接入向导**：「接入新机器」生成一条带一次性配对码的安装命令；在那台机器上执行后自动完成配对
- **中继设置**：粘贴 `autoagents-relay` 启动时打印的连接串即可；也支持直连配对（粘贴 `agentd pair` 打印的配对串）
- 一台已配对的机器可以为另一台电脑上的 AutoAgents 开放配对
- 重命名、立刻重连、移除（断开连接并作废密钥）

规划中：按「工作 / 家庭 / 服务器」分组与自定义标签、重启 agentd、查看机器日志、资源监控曲线。

### 5.3 Agent 管理（Agents）— 已实现

- 按机器分组列出所有 Agent：类型（CLI / 桌面）、Adapter 等级徽章、版本、路径、在线状态
- 在线机器上支持会话的 Agent 可以直接发起托管会话
- **自动发现**：扫描 PATH 与常见安装位置

规划中：列表 / 卡片双视图与筛选、桌面 Agent 的启动 / 停止 / 重启、Agent 详情页。

### 5.4 会话（Sessions）— 已实现

- **托管会话**：应用内直接与 CLI Agent 流式对话，无需开终端；可以选在哪台机器上跑、用哪个模型、在哪个目录
- 消息级元数据：token 用量、费用、耗时
- 会话标出它属于哪个任务，能跳回去
- 实时输出在窗口不可见、事件丢失时也能补上（落库的实时输出）

规划中：终端透传、会话历史检索与导出。

### 5.5 任务中心（Tasks）— 已实现

一条任务从 Issue 或一段需求说明开始，走完下面这条链：

1. **待启动**：选机器、Agent、模型、仓库位置（那台机器上没有仓库可以用 SSH 克隆）；默认「确认后才发 PR」，也可以设成自动开 PR
2. **进行中**：在独立的 git 工作树（`<仓库目录>-wt/<分支>`）里让 Agent 实现；可以旁听会话、可以停止
3. **待确认**：列出改动的文件、逐个看 diff；没有改动就直接结束
4. **提交并创建 PR**：提交前说清楚要往哪个仓库发什么；之后可以追加提交
5. **审查中**：逐项列出 PR 的检查；没过的一键让 Agent 修；合并后标记完成
6. **失败**：原因留在页面上，可以重试

任务页还列出这个任务的分支（或提交、或任务本身）对应的部署，能跳到部署页。

规划中：看板视图、转派其他 Agent、暂停、定时与 Webhook 触发。

### 5.6 工作流编排（Workflows）— 未开始

页面上现在只有四个模板的预览。其中「发布守护流水线」已经不用编排、直接做进了部署页（§5.8），其余三个是规划：

- **修 Issue 流水线**：`claude-cli 分析修复` → `codex-cli 审查 diff` → `grok-cli 撰写变更说明并评论`
- **PR 审查流水线**：新 PR → 双 Agent 交叉审查 → 汇总结论
- **竞争模式**：多 Agent 各自解同一 Issue，人择优 merge

规划中的能力：可视化 DAG 编辑器、变量系统（`{{issue.title}}`、`{{repo}}`…）、运行视图。

### 5.7 GitHub 集成（GitHub）— 已实现

- 认证：PAT（存系统钥匙串）
- 仓库面板：Issue / PR 列表，记住上次选的仓库
- **Issue → 任务**：自动建分支 `agent/issue-12`
- PR：由任务流水线创建或追加；读 PR 状态与检查、取失败检查的日志；取仓库默认分支

规划中：GitHub App、PR 详情页的评论线程、Webhook 监听。

### 5.8 发布与部署（Deploy & Release）— 已实现，未用真实账号验证

面向用户仓库中的 Web 项目（AutoAgents 本身是桌面应用，不需要部署）。支持 **Vercel**、**Cloudflare Pages**、**Cloudflare Workers** 三类项目。

#### 连接

| 平台 | 令牌在哪建 | 需要的范围 |
|---|---|---|
| Vercel | Account Settings → Tokens | 「团队」或「整个账户」；只限单个项目的令牌看不到别的项目 |
| Cloudflare | My Profile → API Tokens | Pages：`Cloudflare Pages: Edit`；还要管 Worker 的构建：再加 `Workers Builds Configuration: Edit`、`Workers Scripts: Read`，并且要建成**用户级**令牌（账户级令牌会被构建接口拒绝） |

令牌只存系统钥匙串。连接后选团队 / 账户，「登记」里从平台拉项目清单勾选。

#### 三类项目各走各的接口

| 操作 | Vercel | Cloudflare Pages | Workers（Builds） |
|---|---|---|---|
| 同步状态 | `GET /v7/deployments`、`GET /v13/deployments/{id}` | `GET …/pages/projects/{name}/deployments` | `GET /accounts/{id}/builds/workers/{tag}/builds` |
| 构建日志 | `GET /v3/deployments/{id}/events` | `GET …/deployments/{id}/history/logs` | `GET /accounts/{id}/builds/builds/{uuid}/logs` |
| 发起 | `POST /v13/deployments`（关联了 GitHub 用 `gitSource` 指定分支 / 提交；否则重做最新一次） | 把这条分支最近的一次 `retry` | 触发 Deploy Hook |
| 重做 | `POST /v13/deployments`（`deploymentId` + `withLatestCommit`） | `POST …/deployments/{id}/retry` | 再触发一次 Deploy Hook |
| 取消 | `PATCH /v12/deployments/{id}/cancel` | **没有这个接口** | `PUT /accounts/{id}/builds/builds/{uuid}/cancel` |
| 升为生产 | `POST /v10/projects/{id}/promote/{deploymentId}` | — | — |
| 回滚 | `POST /v1/projects/{id}/rollback/{deploymentId}` | `POST …/deployments/{id}/rollback` | 按版本重新部署：`POST …/workers/scripts/{name}/deployments` |

#### 状态面板

每分钟从平台拉一次，归成同一种样子。部署记录列出状态、环境、提交、分支、耗时、时间、健康、关联的修复任务；点一行看构建日志。左边清单用颜色点标出每个项目最近一次部署的状态。

在应用里发起部署、重做、取消、升为生产、回滚都有。**动生产环境之前都要先确认**：生产部署（Workers 一律算生产）要勾选确认；回滚要选一次并确认。确认可以按项目关掉。

#### 守护闭环

生产部署到终态之后：

1. **成功** → 访问「生产地址 + 检查路径」，返回 2xx（设了期望内容的话还要包含它）才算健康。超时算不通过；连不上、或者被访问保护挡住（401 / 403）记为「没法判断」，不算失败
2. **构建失败**，或者**健康检查不通过** → 取构建日志，建一个修复任务，并挂到那次部署上

建修复任务要同时满足：项目关联了 GitHub 仓库、开着「生产出问题时自动建修复任务」、这个项目 24 小时内自动建的不到三次。修复任务沿用上次处理这个仓库的机器、Agent 和仓库位置，在那次部署的分支上改；这个仓库还没跑过任务的话，任务建好了但停在「待启动」，由你选机器后启动。改完仍然走任务流水线，要你确认才发 PR。

每一步都进活动记录：部署成功 / 失败、健康检查、建了修复任务、守护为什么没建（没开自动修复、次数用完、没有关联仓库）。

#### 留意

- Vercel 回滚之后会关闭生产域名的自动分配：之后推到生产分支不会再自动上线，要手动把某次部署升为生产；个人版只能回到上一次
- Workers 回滚的是代码版本，对 KV、D1 这些资源的改动不会跟着回退；在应用里触发构建靠 Deploy Hook，构建哪条分支由 Hook 决定
- Pages 不能指定分支发起部署，只能把这条分支最近的一次原样重做
- Worker 构建状态在平台那边的取值文档没有列全，现在是按关键字归类的

规划中：把 `wrangler` 当作 CLI Adapter 由指定机器执行、Webhook 事件代替轮询、部署保护旁路、金丝雀发布、各平台原生的「Fix with agent」之外的其它修复策略。

### 5.9 协同机制（Collaboration）— 未开始

三种协同模式，按需组合（依赖 §5.6 的工作流）：

1. **编排模式（同步）**：工作流引擎按 DAG 调度，Agent 产出直接作为下游输入
2. **GitHub 介质模式（异步）**：Agent A 完成后在 Issue / PR 留下结构化评论，监听该仓库的 Agent B 接续处理 —— 跨机器最稳妥，全程留痕
3. **竞争模式**：N 个 Agent 并行解同一任务，控制端汇总对比，人选最优

### 5.10 可观测性（Activity & Analytics）— 部分

- **已实现**：统一活动流（机器、会话、任务、部署、中继的每一步，按类筛选，跳回原处）；仪表盘的近 24 小时花费
- **规划中**：用量与成本按 Agent / 机器 / 任务 / 时间维度统计；审计视图；机器资源曲线

### 5.11 设置（Settings）— 部分

- **已实现**：中继状态、安装脚本地址、凭据存放说明、清空活动记录、版本信息
- **规划中**：语言与主题、开机自启、agentd 端口、配对设备的撤销列表、自定义 Agent 类型

---

## 6. 关键用户旅程

**旅程 A：从 Issue 到上线，线上挂了自己修（核心场景）**

1. GitHub 页选中 issue #23「登录超时未处理」，点「转任务」
2. 任务里选家里的电脑和它上面的 `claude-cli`，启动；Agent 在独立的工作树里改
3. 做完列出改动，你看完 diff，点「提交并创建 PR」
4. 检查没过，点「让 Agent 修复」；通过后合并
5. 合并触发 Vercel 生产部署；AutoAgents 每分钟对一次状态
6. 部署失败，或者上线后健康检查不过：AutoAgents 取来构建日志，建一个修复任务，任务页和部署记录互相对应
7. 修复任务还是交给家里那台电脑上的 `claude-cli`；改完等你确认再发 PR

**旅程 B：日常纳管**

- 新买服务器 → 机器页「接入新机器」生成安装命令 → 服务器上执行 → 自动配对 → 它上面的 Agent 出现在 Agents 页

**旅程 C：随手对话**

- 会话页选服务器上的 `grok-cli`，直接聊着排查日志，全程无 SSH

---

## 7. 数据模型（现有表）

```sql
tasks(id, title, source, repo, issue_number, issue_url, agent_id, status, branch,
      node_id, adapter, model, workdir, worktree, base_branch, base_sha, head_sha,
      session_id, pr_number, pr_url, detail, failure, instructions, auto_pr, checks,
      created_at, updated_at)
sessions(id, adapter, title, model, workdir, external_ref, node_id, created_at)
messages(id, session_id, role, content, cost_usd, input_tokens, output_tokens, duration_ms, created_at)
custom_models(adapter, model, created_at)
nodes(id, name, relay_url, addr, fingerprint, info, created_at)      -- 配对过的机器；密钥不在这里
settings(key, value)                                                  -- 令牌类不在这里
events(id, ts, kind, level, source, message, link)                    -- 统一事件流
deploy_targets(id, provider, name, platform_id, scope, url, repo, repo_id, git_type, branch,
               framework, prod_requires_approval, auto_fix, health_path, health_contains,
               has_hook, created_at, updated_at)
deployments(id, target_id, provider, project, env, state, raw_state, url, inspector_url,
            branch, commit_sha, commit_message, author, created_ms, building_ms, ready_ms,
            error_code, error_message, rollback_ok, current, guarded, health, task_id, updated_at)
```

凭据（GitHub PAT、中继令牌、Vercel / Cloudflare 令牌、Deploy Hook 地址、与各台机器配对的密钥）都在系统钥匙串里。

规划中尚未建表：`workflows`、`workflow_runs`、按 Agent / 机器 / 项目的用量汇总。

---

## 8. 技术栈与工程约定

| 层 | 选型 |
|---|---|
| 桌面框架 | **Tauri 2**（Rust core） |
| 应用核心 | Rust：tokio、serde、rusqlite（内置 SQLite）、reqwest（GitHub / Vercel / Cloudflare REST）、keyring（系统钥匙串） |
| 共享核心 `crates/core` | Agent 适配器、节点间链路、中继协议、端到端加密（x25519-dalek、chacha20poly1305、HKDF、HMAC、SHA-2）、TLS（rustls、rcgen 自签证书）、WebSocket（tokio-tungstenite）、机器指标（sysinfo） |
| 节点程序 `crates/node` | `agentd`（tokio + tokio-rustls）与 `autoagents-relay`，同一个 crate 的两个可执行文件 |
| 前端 | **React 19 + TypeScript + Vite**、**Tailwind CSS v4**、TanStack Query、React Router、Zustand |
| IPC | Tauri `invoke` + `event`（进度流推送） |
| 测试 | Rust 单元与集成测试（`cargo test --workspace`）；界面测试（无头 WebKit + 模拟的 Tauri 接口，四套共 235 项）；CI 在 Linux 与 macOS 上跑 |
| 发布 | 打 `v*` 标签 → GitHub Actions 构建 `agentd` / `autoagents-relay`（Linux x86_64 / arm64 静态链接，macOS Intel / Apple Silicon）→ GitHub Releases，附 SHA256SUMS |
| 许可证 | **GPL-3.0**：应用全部源码（Rust 核心、agentd、中继、前端）以 GPL-3.0 发布 |

约定：

- 后台任务一律用 `tauri::async_runtime::spawn`；不要在持有 `std::sync::Mutex` 守卫的时候 `.await`
- 界面上展示给用户的顺序（排序）要固定地区：`localeCompare` 不带地区的话，macOS 与 Linux 的结果不一样
- 事件用 `模块.动作` 命名，新增一种就在前端的 `events.ts` 里补一条中文文案

---

## 9. 版本路线图

| 里程碑 | 范围 | 状态 |
|---|---|---|
| **M1 单机版** | 本机 Agent 发现；CLI Agent 托管会话；GitHub PAT，Issue → 任务 → PR 全流程 | 已完成 |
| **M2 多机** | agentd + 配对 + 中继 / 直连；机器监控；远程会话；安装脚本与发布 | 已完成 |
| **M3 协同** | 部署跟踪与守护（已完成）；工作流 DAG 编辑器与运行时；GitHub 介质异步协同；竞争模式；Webhook 触发 | 进行中 |
| **M4 深水区** | 主动部署与回滚、发布守护闭环（已完成）；桌面 Agent 深度控制；用量成本分析；自动化规则引擎；自更新 | 进行中 |

---

## 10. 风险与对策

| 风险 | 对策 |
|---|---|
| CLI Agent 接口频繁变动（如 claude / codex 参数变动） | Adapter 层隔离；输出协议解析有独立的测试 |
| 跨网络连通（NAT、无公网 IP） | 被管理的机器只出站；自建中继穿透 NAT；有公网地址时可以直连 |
| 中继被别人架着看流量 | 中继只转发密文；端到端加密的密钥只在控制端与各台机器上；中继可以自己架 |
| Agent 产出质量不可控直接写仓库 | 独立工作树 + 分支隔离 + 写操作人工卡点（确认后才提交 / 推送 / 开 PR） |
| 自动发布殃及生产环境 | 生产操作先确认；健康检查；回滚兜底 |
| 守护建的修复任务停不下来 | 一个项目 24 小时内最多三次；修复任务改完仍要人工确认才发 PR |
| 各 Agent 用量计费口径不一 | 费用是「估算」，口径随各家 Agent |
| 平台接口变了 | 两家平台的接口都对照官方文档核实过，并有照着接口形状搭的模拟平台跑全流程；**尚未用真实账号验证** |

---

## 11. 对标（2026 年 11 月）

读了 24 个同类产品的公开文档（README、官方文档），下面是最接近的十个。「有 / 没有」指在读到的页面里有没有找到明确说明，没找到不等于一定没有。完整笔记见调研原文。

| 产品 | 形态 | 管自己的多台机器 | 远程接入 | 端到端加密 | 多种 CLI Agent | Issue → PR | 检查 / 评论交回 Agent | 跟踪部署并修复 |
|---|---|---|---|---|---|---|---|---|
| Conductor | Mac 应用 + iOS | 否 | 官方中继 | 有 | 有 | 有 | 部分（人点发送） | 只列出部署 |
| Vibe Kanban | 本地 Web（原作者公司已关闭，社区维护） | 否 | 自己搭隧道 | 未说明 | 有（10+） | 有 | 部分（行内评论） | 没有 |
| Claude Squad | 终端 TUI | 否 | — | — | 有 | 否 | 没有 | 没有 |
| Emdash | 三平台桌面应用 | 有（SSH） | SSH | 靠 SSH | 有 | 有 | 只看 CI 检查 | 没有 |
| Nimbalyst | 三平台桌面 + iOS | 否 | 厂商同步服务 | 有（个人同步） | 有 | 部分 | 应用内审查 | 没有 |
| Orchestrator.inc | 三平台桌面 + 手机 | 有 | Cloudflare 隧道 | 无（Cloudflare 能看到） | 有（40+） | 有 | 有（CI 失败、评论） | 没有 |
| T3 Code | 桌面 + Web + 手机 | 有 | 局域网 / Tailscale / 厂商中继 | 未说明 | 有（11+，任何 ACP） | 部分 | 部分 | 只看 CI 日志 |
| Happy | 桌面 + 手机 + Web | 有 | 中继，可自建 | 有 | 有 | 没有 | 没有 | 没有 |
| CloudCLI | 本地 Web | 否 | 自己搭隧道，无内置鉴权 | 没有 | 有 | 部分 | 没有 | 没有 |
| OpenHands | 自托管 Web | 否 | — | 没有 | 有 | 部分 | 没有 | 没有 |
| **AutoAgents** | 桌面应用 | **有（自建中继 / 直连）** | **自建中继或直连** | **有** | **有** | **有** | 部分（一键让 Agent 修） | **有（Vercel / Cloudflare）** |

同类产品里最常见的 15 种能力（在读到说明的产品里数）：

| 能力 | 家数 |
|---|---|
| 跑在自己的机器或服务器上 | 20 |
| 每个任务隔离（worktree / 分支 / 沙箱） | 14 |
| 托管的云端执行 | 11 |
| 驱动多种第三方 CLI Agent | 10 |
| 手机访问 | 10 |
| 应用内看 diff | 9 |
| 从应用合并 | 9 |
| 定时 / 事件自动化 | 9 |
| Issue → Agent → PR | 8 |
| 内置中继或隧道 | 6 |
| 扫码或一次性码配对 | 6 |
| PR 评论交回 Agent | 5 |
| 手机上批准确认 | 5 |
| Agent 需要你时推送通知 | 5 |
| 应用内预览浏览器 | 5 |

少见的：同时管自己的多台机器（4 家）、用量与成本面板（4 家）、远程流量端到端加密（3 家）、CI 失败自动交给 Agent（2 家）、发布后验证部署并修复（没有读到）。

### 可以占住的位置

1. **自建中继 + 端到端加密 + 自己的多台机器 + 多种 CLI + PR 流水线 + 部署守护，在一个产品里。** 读到的文档里没有第二家把这些做完：Conductor 加密但只有 Mac 且用官方中继；Happy 加密、可自建，但没有 worktree 和 PR 流水线；Orchestrator.inc 和 T3 Code 跨机器但经 Cloudflare；Emdash 跨机器只靠 SSH。
2. **合并之后的事。** 只有 Orchestrator.inc 和 Copilot 会把 CI 失败自动交给 Agent；没有读到谁在合并后验证 Vercel / Cloudflare 的部署。
3. **同一任务多次尝试、并排比较。** 只找到 Pragma 一家。
4. **跨机器、跨提供商的成本。** 四家有展示，多数只看一台机器。
5. **机器健康与 Agent 版本漂移。** 只看到 Emdash 显示单机的运行时状态。
6. **不依赖厂商账号也能远程。** T3 免费账户限 3 台；Orchestrator.inc 的固定地址要账号。
7. **Windows 与 Linux 桌面版同等可用。** Conductor 只有 Mac；Cline 桌面版只有 Mac 和 Windows。

### 已经照做的

- 一次性配对码，10 分钟有效（Conductor、Orchestrator.inc、T3 Code、Happy 都这么做）
- 粘贴兜底：中继连接串、直连配对串都能直接粘
- 一条命令接入没有界面的机器（Orchestrator.inc、Harness）
- 明确说中继能看到什么（Orchestrator.inc 的做法）
- 动生产环境之前先确认；自动修复设上限

### 还没做、值得做

- PR 的评论线程一键交回 Agent（Conductor、Orchestrator.inc）
- CI 失败集合变了才交给 Agent，只给名字和链接而不是整段日志（Orchestrator.inc）
- 一个「可以合并了吗」面板：git 状态、PR、CI、部署、评论线程（Conductor）
- 手机访问、推送通知、手机上批准确认
- 按机器 / Agent / 项目的用量与成本
- 一键导出全部数据（开放的数据格式）

出处：

- Conductor：[远程控制](https://www.conductor.build/docs/reference/remote-control)、[Checks](https://www.conductor.build/docs/reference/checks)
- Vibe Kanban：[README](https://github.com/BloopAI/vibe-kanban)
- Claude Squad：[README](https://github.com/smtg-ai/claude-squad)
- Emdash：[README](https://github.com/generalaction/emdash)、[远程开发](https://emdash.com/docs/remote-development)
- Nimbalyst：[README](https://github.com/Nimbalyst/nimbalyst)、[安全](https://nimbalyst.com/docs/open-safe-private-secure/security.md)
- Orchestrator.inc：[README](https://github.com/OrchestratorInc/agent-orchestrator)、[远程访问](https://docs.orchestrator.inc/configuration/remote-access/)、[CI 恢复](https://docs.orchestrator.inc/guides/ci-recovery/)
- T3 Code：[README](https://github.com/pingdotgg/t3code)、[远程访问](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md)
- Happy：[README](https://github.com/slopus/happy)、[桌面端](https://github.com/slopus/happy-desktop)
- CloudCLI：[README](https://github.com/siteboon/claudecodeui)
- OpenHands：[README](https://github.com/OpenHands/OpenHands)

# AutoAgents

跨机器、多 Agent 的统一管理与协同工作台。

把工作电脑、家庭电脑、服务器上的 AI Agent —— CLI 类（Claude CLI / Codex CLI / Grok CLI / Pi）与桌面类（ZCode / Kimi Code / Grok Bot 等）—— 纳管到一个桌面应用里，跨互联网调度与协同：处理 GitHub 仓库里的 Issue 与 PR，并跟踪发布到 Vercel / Cloudflare 的每一次部署，线上出问题时让 Agent 去修。

```
                ┌──────────────────────────────────┐
                │  AutoAgents 桌面应用（Rust + React）│  管理端：调度、看板、密钥（系统钥匙串）
                └────┬─────────────────────────┬───┘
       端到端加密  │                  端到端加密 │
          ┌───────▼──────┐              ┌───────▼──────┐
          │ autoagents-  │  只验令牌、   │  agentd      │  被管理的机器：发现 Agent、
          │ relay 中继   │  转发密文     │  工作电脑 / 家庭电脑 / 服务器
          └──────────────┘              └──────────────┘
```

所有被管理的机器都只向外发起连接，放在公司网络或家庭路由器后面也能接入；中继看不到任何内容。

## 现在能做什么

**Agent 与会话**

- 扫描本机的 PATH 与 `/Applications`，识别 CLI Agent 与桌面 Agent；其它机器连上后同步它的清单，按机器分组
- 在应用内直接与 Claude CLI / Codex CLI / Grok CLI / Pi 流式对话，多轮续聊、随时停止；会话可以开在任意一台在线的机器上
- 模型清单从各 Agent 自己的配置里读（比如经网关接入的模型），可以搜索，也可以手填

**多机接入**

- 每台被管理的机器装一个 `agentd`（单个可执行文件，Linux 静态链接 / macOS），一条安装命令接入，注册成开机自启的服务
- 自建中继或直连两种方式；配对码一次一用、10 分钟有效，配对后双方各留一把长期密钥
- 机器页看在线状态、延迟、CPU / 内存、运行时长、agentd 版本；可以重命名、移除（移除即作废它的密钥）

**GitHub 与任务流水线**

- PAT 连接（存系统钥匙串），浏览仓库的 Issue / PR，Issue 一键转任务，也可以直接写一段需求建任务
- 启动任务时选机器、Agent、模型和仓库位置（那台机器上没有仓库的话可以用 SSH 克隆），Agent 在独立的 git 工作树里干活
- 做完后列出改动的文件、逐个看 diff，你确认了才提交、推送、开 PR（也可以设成自动开 PR）
- 跟踪 PR 的检查，没过的一键让 Agent 修，合并后标记完成

**工作流编排（多 Agent 协同）**

- 三个内置模板，建好即用：**修 Issue 流水线**（一个 Agent 修复，第二个交叉审查，第三个撰写变更说明）、**PR 审查流水线**（两位 Agent 并行审查同一个 PR，第三人汇总结论）、**竞争模式**（两个 Agent 并行解决同一 Issue、各自一条分支，人工评审择优）
- 步骤是一张依赖图：没有依赖的步骤同时开工（同一台机器上也能并排跑几个 Agent），依赖满足的上一步的产物自动拼进下一步的提示词——审查步骤拿得到工作树里的 diff，汇总结论拿得到两位审查者的意见
- 任务步骤走的是同一条任务流水线：每步一条专用分支（`agent/issue-7-w1`），做完停在「改动待确认」，往外发仍然要你逐个确认
- 竞争模式停在人工评审门：你在候选里选定一个方案，其余保持原样；一路失败不会把评审门堵死
- 失败的步骤可以整批重跑（建新任务，旧的失败记录留在任务页）；取消会把正在跑的任务和会话一并停下
- 每一步的动态（开始、完成、失败、跳过、等你决策）都进活动流，链回工作流页

**自动化规则（盯 GitHub，条件满足就自己开工）**

- **Issue 打标签**：仓库的 Issue 打上指定标签，自动建「修 Issue 流水线」（或竞争模式）并启动——修完直接开 PR 也行，验收就变成看 PR
- **新 PR**：仓库来了新 PR（草稿不算），自动跑 PR 审查流水线，汇总结论贴成 PR 评论
- **PR 检查失败**：任务开出去的 PR 检查红了，自动让原来那个任务的 Agent 在同一个会话里继续修，修完照旧走收尾（开着自动 PR 的会再推上去，还红会再修）
- 机器、每步的 Agent、模型、仓库位置在建规则时配好；没配的项沿用这个仓库上次任务用过的；检查失败规则连这些都不用——全沿用任务自己的
- 每 2 分钟扫一遍（也可以点「立即检查」）；一条 Issue / 一个 PR / 一个提交上的红检查只自动处理一次，一条规则一天最多触发十次
- 规则可随时停用、启用、删除；触发记录和失败原因都进活动流

**决策中心（只管拍板）**

- 侧边栏「决策」页把所有等你拍板的事聚到一处：竞争模式的人工评审、改动待确认的任务、还没开工的任务、PR 已开等验收的——侧边栏挂着待办数的角标，来新的事会发系统通知
- PR 验收一个按钮：检查状态一目了然，点「合并」即合并 PR，任务自动标完成

**部署与守护（Vercel / Cloudflare）**

- 连接 Vercel 和 Cloudflare（令牌存钥匙串），从平台上登记项目：Vercel 项目、Cloudflare Pages 项目、Workers
- 每分钟从平台拉一次部署：状态、环境、提交、分支、耗时、地址、健康；点一行看构建日志
- 在应用里发起部署、重做、取消、把预览升为生产、回滚；动生产环境之前都要先确认
- 守护：生产部署失败，或者上线后健康检查不过，就取来构建日志自动建一个修复任务——这个仓库之前跑过任务的话，沿用那次的机器和 Agent 直接开工，没有的话停在「待启动」等你选；可以按项目设置「修完直接开 PR」，验收就变成在决策中心点合并
- 任务页列出这个任务的分支对应的部署，部署记录链回为它建的修复任务

**可观测与运维**

- 仪表盘：在线机器、进行中的对话、近 24 小时花费与近几天的趋势、GitHub 连接状态
- 活动页：机器、会话、任务、部署、中继的每一步都有记录，按类筛选，能跳回原处

## 多机接入

1. 在一台有公网地址的机器上装中继（7000 端口要能从外面访问）：

   ```bash
   curl -fsSL https://raw.githubusercontent.com/1989tianlong/AutoAgents/main/scripts/install-relay.sh | sh
   autoagents-relay --public wss://<公网IP或域名>:7000
   ```

   它会打印一行 `autoagents://relay?addr=…&token=…&fp=…`。自带 TLS（自签名证书，按指纹钉住），不需要域名；已有域名和证书的话可以加 `--no-tls` 放在 Caddy / Nginx 后面。

2. AutoAgents → 机器 → 中继设置，把那一行粘进去。
3. 机器 → 「＋ 接入新机器」，得到一条带一次性配对码的安装命令，到另一台机器上执行：

   ```bash
   curl -fsSL https://raw.githubusercontent.com/1989tianlong/AutoAgents/main/scripts/install-agentd.sh | sh -s -- install \
     --name <名字> --relay wss://… --token <令牌> --pair <配对码> [--fp <证书指纹>]
   ```

   脚本从 GitHub Releases 下载对应平台的 agentd 并核对 SHA-256，装到 `~/.autoagents/bin`，注册成开机自启的服务。它连上中继后，AutoAgents 这边自动完成配对。

被管理的机器自己有公网地址、不想架中继的时候可以直连：在那台机器上 `agentd --listen 0.0.0.0:7100`，再执行 `agentd pair`，把它打印的配对串粘到 AutoAgents 里。

## 部署与守护

1. 部署页填入 API 令牌。令牌在平台上自己建，只存进系统钥匙串：
   - **Vercel**：Account Settings → Tokens。范围选「团队」或「整个账户」——只限单个项目的令牌看不到别的项目。
   - **Cloudflare**：My Profile → API Tokens。只管 Pages 要 `Cloudflare Pages: Edit`；还要管 Worker 的构建，再加 `Workers Builds Configuration: Edit` 和 `Workers Scripts: Read`，并且要建成**用户级**令牌（账户级令牌会被构建接口拒绝）。
2. 选团队 / 账户，「＋ 登记」里勾选项目。Workers 在平台上没有关联仓库，登记时（或之后在设置里）填它的生产地址和 Deploy Hook 地址。
3. 之后就是自动的：每分钟同步一次部署状态，生产部署成功后访问「生产地址 + 检查路径」，返回 2xx（设了期望内容的话还要包含它）才算健康。

守护建修复任务要同时满足：项目关联了 GitHub 仓库、开着「生产出问题时自动建修复任务」、这个项目 24 小时内自动建的不到三次。修复任务沿用上次处理这个仓库的机器、Agent 和模型，在那次部署的分支上改；这个仓库还没跑过任务的话，修复任务停在「待启动」，到任务页选好机器再启动。

几家平台的接口并不对等，用的时候留意：

- **Cloudflare Pages** 没有取消部署的接口，也不能指定分支发起部署（只能把这条分支最近的一次原样重做）
- **Vercel** 回滚之后会关闭生产域名的自动分配：之后推到生产分支不会再自动上线，要手动把某次部署升为生产
- **Workers** 回滚的是代码版本，它对 KV、D1 这些资源的改动不会跟着回退；在应用里触发构建靠 Deploy Hook，构建哪条分支由 Hook 决定

## 安装与开发

```bash
npm install
npm run tauri dev     # 本地开发
npm run tauri build   # 打包安装包
```

要求 Node 20+、Rust stable。桌面应用目前在 macOS 上开发和验证。

### 测试

```bash
npm run test:rust     # Rust：Agent 输出协议、任务流水线、部署与守护、多机联调
npm run test:scripts  # 两个安装脚本（替身的 curl 假装 GitHub Releases，不联网）
npm run test:release  # 发布流程里的脚本空跑一遍（替身的 gh，不发布；需要 PyYAML）
npm run dev           # 另开一个终端：界面测试需要前端开发服务器
npm run test:ui       # 会话、机器、任务、部署、工作流、自动化六套界面测试（无头 WebKit，模拟 Tauri 接口）
```

`test:ui` 依赖 `playwright-core` 及其 WebKit：`npm i -D playwright-core && npx playwright-core install webkit`；
也可以用环境变量 `PLAYWRIGHT_CORE` 指向本机已有的 playwright-core 目录。

CI（`main` 分支每次推送）：Rust 测试和安装脚本测试跑 Linux 与 macOS，界面测试跑 Linux。

打 `v*` 标签会走一遍发布：构建 `agentd` 与 `autoagents-relay`（Linux x86_64 / arm64 静态链接，macOS Intel / Apple Silicon）→ 用构建出来的程序把多机联调走一遍 → 发到 GitHub Releases（附 SHA256SUMS）→ 再按安装命令在 Linux 和 macOS 上各装一遍。

## 代码结构

| 位置 | 内容 |
| --- | --- |
| `src-tauri/` | 桌面应用：Rust 核心（会话、机器与中继、任务流水线、GitHub、部署与守护、用量）与 React 前端的壳 |
| `src/` | 前端：React 19 + TypeScript + Tailwind CSS v4 |
| `crates/core/` | 与界面无关的核心：Agent 适配器、端到端加密、中继协议、节点间链路 |
| `crates/node/` | `agentd`（装在被管理的机器上）与 `autoagents-relay`（中继） |
| `scripts/` | 两个安装脚本 |
| `docs/product-design.md` | 产品设计、实现状态与对标 |
| `docs/research/` | 对标与部署守护的调研笔记（英文原文，带出处） |

## 路线图

| 里程碑 | 内容 | 状态 |
| --- | --- | --- |
| M1 | 本机 Agent 发现与托管会话；模型选择；GitHub 接入；Issue → 任务 → PR | 已完成 |
| M2 | agentd 多机接入（中继 / 直连）、配对与端到端加密；机器监控；远程会话 | 已完成 |
| M3 | 部署跟踪与守护（Vercel / Cloudflare）；工作流 DAG 编排；多 Agent 协同 | 已完成 |
| M4 | 主动部署与回滚；桌面 Agent 深度控制；用量成本分析；自动化规则 | 自动化规则（Issue 打标签 / 新 PR / 检查失败自动修）与决策中心已完成，其余未开始 |

## 对标

2026 年 11 月看了二十多个同类产品的公开文档，最接近的几个：

| 产品 | 管自己的多台机器 | 端到端加密的远程接入 | 驱动多种 CLI Agent | Issue → PR | 跟踪并修复线上部署 |
| --- | --- | --- | --- | --- | --- |
| Conductor | 只有 Mac 本机 | 有（官方中继） | 有 | 有 | 只列出部署 |
| Happy | 有 | 有（可自建中继） | 有 | 没有 | 没有 |
| Emdash | 有（SSH） | 靠 SSH | 有 | 有 | 只看 CI 检查 |
| Orchestrator.inc | 有 | 无（经 Cloudflare） | 有 | 有 | CI 失败交回 Agent，不看部署 |
| T3 Code | 有 | 未说明 | 有 | 部分 | 只看 CI 日志 |
| **AutoAgents** | **有（自建中继 / 直连）** | **有（可自建中继）** | **有** | **有** | **有：Vercel / Cloudflare 部署失败或健康不过时自动建修复任务** |

把「自己人的几台机器 + 端到端加密 + 多种 CLI + PR 流水线 + 部署之后有人管」做完的，读到的文档里没有第二家。表里的「没有」指在读到的文档里没找到，不等于一定没有。完整的对照表与出处见 [docs/product-design.md](docs/product-design.md)。

## 已知限制

- 部署部分按两家平台的官方接口实现，用照着接口文档搭的模拟平台测过全流程，**还没有拿真实账号跑过**；Worker 构建状态在平台那边的取值文档没有列全，现在是按关键字归类的
- 桌面应用只在 macOS 上验证过；Windows 上没有 agentd（可以装在 WSL 里）
- 工作流的一次运行里所有步骤共用一台机器（每一步可以选不同的 Agent），还做不到按步骤指定不同机器
- 自动化规则靠每 2 分钟轮询 GitHub（受接口速率限制），不是 Webhook 实时推送；新 PR 规则对同一 PR 只审一次，后续新提交不会自动重审
- 桌面 Agent 的深度控制、主动部署与回滚还没做

## 许可证

[GPL-3.0](LICENSE)

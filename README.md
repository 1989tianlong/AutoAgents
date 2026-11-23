# AutoAgents

跨机器、多 Agent 的统一管理与协同工作台。

把工作电脑、家庭电脑、服务器上的 AI Agent —— CLI 类（Claude CLI / Codex CLI / Grok CLI / Pi）与桌面类（ZCode / Kimi Code / Grok Bot 等）—— 纳管到一个桌面应用里，远程调度与协同，处理 GitHub 仓库中的 code / issues / pull requests，并自动发布到 Vercel / Cloudflare。

## 功能路线

| 里程碑 | 内容 |
| --- | --- |
| M1 | 本机 Agent 发现与纳管；CLI Agent 托管会话；GitHub 接入，Issue → 任务 → PR |
| M2 | `agentd` 节点多机接入（mDNS / Tailscale / 中继）；机器监控；远程会话 |
| M3 | 工作流 DAG 编排；多 Agent 协同（GitHub 介质异步协同 / 竞争模式）；部署状态监听与守护 |
| M4 | 桌面 Agent 深度控制；主动部署与回滚；用量成本分析；自动化规则 |

完整设计见 [docs/product-design.md](docs/product-design.md)。

## 当前可用（M1 进行中）

- **本机 Agent 发现**：扫描 PATH 与 `/Applications`，识别 CLI Agent 与桌面 Agent
- **托管会话**：在应用内直接与 Claude CLI / Codex CLI / Grok CLI / Pi 对话，流式输出、多轮续聊、可随时停止
- **模型选择**：按 Agent 从各自的本机配置读取模型清单（如经 magpie 网关接入的模型），支持搜索，也可以直接填写清单之外的模型 ID
- **GitHub**：PAT 连接（存系统 Keychain），浏览仓库的 Issue / PR，Issue 一键转任务

## 技术栈

- 桌面框架：[Tauri 2](https://tauri.app)（Rust 核心，启动快、资源占用低）
- 前端：React 19 + TypeScript + [Tailwind CSS v4](https://tailwindcss.com)
- 状态库：本地 SQLite

## 开发

```bash
npm install
npm run tauri dev     # 本地开发
npm run tauri build   # 打包安装包
```

要求：Node 20+、Rust stable。

### 测试

```bash
npm run test:rust     # Rust 单元测试：Agent 输出协议解析、模型清单读取
npm run dev           # 另开一个终端：界面测试需要前端开发服务器
npm run test:ui       # 会话页界面测试（无头 WebKit，模拟 Tauri 接口）
```

`test:ui` 依赖 `playwright-core` 及其 WebKit：`npm i -D playwright-core && npx playwright-core install webkit`；
也可以用环境变量 `PLAYWRIGHT_CORE` 指向本机已有的 playwright-core 目录。

## 许可证

[GPL-3.0](LICENSE)

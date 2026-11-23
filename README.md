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

## 技术栈

- 桌面框架：[Tauri 2](https://tauri.app)（Rust 核心，启动快、资源占用低）
- 前端：React 19 + TypeScript + [Tailwind CSS v4](https://tailwindcss.com)
- 状态库：本地 SQLite（规划）

## 开发

```bash
npm install
npm run tauri dev     # 本地开发
npm run tauri build   # 打包安装包
```

要求：Node 20+、Rust stable。

## 许可证

[GPL-3.0](LICENSE)

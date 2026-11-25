> 调研笔记原文（英文），2026-11-23。结论与取舍已整理进 [../product-design.md](../product-design.md) 第 11 节；这里保留出处与逐条依据。

# Competitors: multi-agent orchestrators and remote access to coding agents

Researched 2026-11-23 from public READMEs (fetched raw from GitHub), vendor docs and search results. CC = Claude Code.

Legend for Section A: **Y** stated on a page I read · **P** partial or manual · **N** not found on the pages I read (not proof of absence) · **U** unverified (search snippet or third-party only) · **—** not applicable.

## A. Feature matrix

| Product | Platforms | Agents driven | Isolation | Parallelism | Remote / multi-machine | E2E enc. | Issue→PR | Review/CI loop | Diff / rewind | Cost / usage | Ops | Mobile |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Conductor | Mac; iOS (exp.) | CC, Codex, Cursor, OpenCode | worktree; no sandbox | many; manual fan-out | E2E relay to Mac; Vercel cloud | Y | Y GitHub/Linear | P: comments/CI shown, user sends | diff, comments→agent, checkpoints | Y Usage page | Routines, Plan mode, bundled CLIs | iOS |
| Vibe Kanban (sunsetting) | web via npx; self-host | 10+ CLIs | worktree | many; kanban | server + user tunnel | U | Y kanban issue→PR | P inline comments→agent | diff, preview | N | auto cleanup | N |
| Claude Squad | TUI | CC, Codex, Gemini, Aider | tmux + worktree | many | N documented | N | N (push branch) | N | diff tab | N | --autoyes | N |
| Emdash Classic | Mac/Win/Linux | 10 CLIs | worktree | many | SSH + Workspace Server | SSH only | Y 9 trackers→agent | P: CI checks view | diff view | Y subscription usage | cron, tmux | N |
| Nimbalyst (ex-Crystal) | Mac/Win/Linux; iOS | Codex, CC, OpenCode, Copilot | worktree | P | vendor sync server | Y personal sync | N | P in-app PR review | red/green diff | Y usage report | push, auto-update | iOS |
| Orchestrator.inc | Mac/Win/Linux; iOS/Android; cloud | 40+ | worktree | orchestrator→workers | cloudflared tunnel; remote hosts | N (Cloudflare sees TLS) | Y GitHub/GitLab | Y CI + comments→owning worker | Files diff, reviewer agents | N | cron, push, self-update | iOS/Android |
| T3 Code | Mac/Win/Linux; web; iOS/Android | 11 + any ACP | worktree per thread | P | LAN/Tailscale/T3 Connect; linked machines | U | P (Issues doc unread) | P PR page with CI logs | diff, line comments | N | scheduled tasks, scopes | iOS/Android/web |
| Happy | Mac/Win/Linux; iOS/Android/web | Claude, Codex, Grok, Gemini, ACP | plain folders | P | E2E relay, self-hostable | Y | N | N | diffs, /review | Y live cost | permission modes, daemon | iOS/Android/web |
| CloudCLI | web; tray app | CC, Cursor CLI, Codex | host; Docker sandbox (exp.) | P | VPS + user tunnel; no built-in auth | N | P plugin | N | git explorer | P plugin | plugins | browser |
| Claude Code Remote Control | CLI; Claude app; cloud | CC only | cloud sandbox | 32 (server mode) | via Anthropic API | N (TLS) | N | N | N | N | push; cron in Managed Agents | iOS/Android |
| Omnara (pivoted) | web, CLI, API, Slack | own agent loop | sandbox / machine | Y | omnarad dials out | N | P examples | P PR-review example | N | N | permissions, cron | legacy only |
| Cursor Cloud Agents | cloud; iOS; web; Slack | Cursor agent | VM + branch | unlimited | vendor cloud | — | Y @cursor on issue | P @cursor on PR | artifacts | N | secrets, domain allow-list | iOS; Android PWA |
| Copilot coding agent | GitHub | Copilot agent | branch; Actions VM | Y | vendor | — | Y assign issue, failing runs | Y @copilot, Watch and fix | N | N | automations; 59-min cap | N |
| OpenAI Codex cloud | web, desktop, mobile, CLI | Codex | workspace per task | U | vendor cloud | — | U | U | N | N | published environments | Y |
| Jules | web | Gemini | cloud VM | 3 / 15 / 60 | vendor cloud | — | Y issue label | N | diff approve | N | plan approval | N |
| Amp | CLI, desktop, web, mobile | Amp | orb (VM) per thread | Y | vendor cloud | — | N | N | N | N | Slack start; notifications | iOS/Android |
| OpenHands Canvas | web (self-host) | OpenHands, CC, Codex, Gemini, Pi, ACP | Docker (opt.) | Y | own servers | N | P issue→tasks | N | N | N | automations | N |
| Symphony (OpenAI) | spec + Elixir reference | Codex | isolated run | Y | own infra | — | Y board→PR | U | N | N | polling service | N |
| Cline | IDE, CLI, desktop Mac/Win, SDK | Cline | N | teams | N | — | N | N | diff, checkpoints | N | cron, messaging | N |
| opencode | CLI/TUI, desktop beta | opencode | N | N | N | — | N | N | N | N | plan/build modes | N |
| Kilo Code | VS Code, JetBrains, CLI, cloud | Kilo | N | N | N | — | N | P PR-review product | N | N | kilo run --auto | N |
| Goose | desktop 3 OS, CLI | goose | N | N | N | — | N | N | N | N | ACP backend | N |
| Aider | terminal | aider | N | N | N | — | N | N | git diff | N | auto-commit, lint/test | N |
| Gemini CLI | CLI | Gemini | N | N | N | — | N | N | N | N | plan mode (U) | N |

## B. Per-product facts

**Conductor** (closed source, v0.98.0)
- macOS only: "Conductor is not available for Windows or Linux yet." [install](https://www.conductor.build/docs/installation)
- Runs CC, Codex, Cursor and OpenCode with your existing logins; "Agents run directly on your system without sandboxing." [faq](https://www.conductor.build/docs/faq)
- Remote Control (experimental): the iOS app drives local Mac workspaces. "Traffic between your iPhone and your Mac is end-to-end encrypted. It passes through Conductor's relay, which can't read it. Each pairing code works once and expires after 10 minutes." [remote-control](https://www.conductor.build/docs/reference/remote-control)
- The Checks tab holds git status, PR, CI, deployments, review threads and todos, with "Send unresolved comments to the agent" and merge blockers. Checkpoints snapshot each turn in a private Git ref for one-click revert. [checks](https://www.conductor.build/docs/reference/checks) · [checkpoints](https://www.conductor.build/docs/reference/checkpoints)
- Usage page (2026-11-20): chats, estimated cost and tokens for 7/30/90 days by model and workspace. [changelog](https://www.conductor.build/changelog/0.98.0-see-where-your-usage-goes)
- Cloud workspaces on Vercel sandboxes outlive the app. Conductor announced it was joining Meta on 2026-11-04 with a cloud shutdown on 2026-11-20, then posted "Conductor is back" on 2026-11-13; current cloud status is U. [cloud faq](https://www.conductor.build/docs/cloud/faq) · [changelog](https://www.conductor.build/changelog/0.93.0-conductor-is-joining-meta)

**Vibe Kanban** (BloopAI, Apache-2.0, 28.8k stars) — sunsetting
- bloop shut down on 2026-04-10; the project continues "open source and community maintained" with a "fully local architecture". [shutdown](https://www.vibekanban.com/blog/shutdown)
- `npx vibe-kanban`: kanban issues, a workspace per task with its own branch, terminal and dev server, and a git worktree per attempt. 10+ agents including CC, Codex, Gemini CLI, Copilot, Amp, Cursor and OpenCode. [readme](https://github.com/BloopAI/vibe-kanban)
- "Review diffs and leave inline comments — send feedback directly to the agent without leaving the UI"; built-in browser; AI-written PRs; merge. [readme](https://github.com/BloopAI/vibe-kanban)
- Remote use means a server behind Cloudflare Tunnel or ngrok; a "relay tunnel mode" appears only as env vars (U). [readme](https://github.com/BloopAI/vibe-kanban)

**Claude Squad** (smtg-ai, AGPL-3.0, 8.9k stars)
- TUI (`cs`) for CC, Codex, Gemini and Aider; needs tmux and gh. [readme](https://github.com/smtg-ai/claude-squad)
- "tmux to create isolated terminal sessions for each agent; git worktrees to isolate codebases". [readme](https://github.com/smtg-ai/claude-squad)
- Attach to re-prompt, `s` commit and push, `c` commit and pause, `r` resume; diff tab; `--autoyes` is experimental. [readme](https://github.com/smtg-ai/claude-squad)
- The README documents no PR automation, cost tracking, mobile client or relay.

**Emdash Classic** (generalaction, Apache-2.0, 6.3k stars)
- Free desktop app for macOS, Windows and Linux; worktree per task; CC, Codex, Cursor, OpenCode, Gemini, Amp and more. [readme](https://github.com/generalaction/emdash)
- Multi-machine over SSH/SFTP: a Workspace Server on the remote machine handles "files, Git, terminals, agents, search and automations" with no public server port; it reads `~/.ssh/config` aliases and ProxyJump. [remote](https://emdash.com/docs/remote-development) · [ssh](https://emdash.com/docs/remote-development/ssh-connections)
- Settings → Machines shows connection, resource usage and runtime updates; "Subscription readings are available in Settings → Usage; shared accounts appear once across machines." [machines](https://emdash.com/docs/remote-development/machines)
- Cron automations run on the project's machine, so remote schedules "don't require the desktop to remain connected". [automations](https://emdash.com/docs/automations)
- Issues from Linear, GitHub, Jira, GitLab and more feed agents; PRs and CI checks in-app; merge. "Emdash 2.0, coding agents for your whole team, is coming soon." [readme](https://github.com/generalaction/emdash) · [blog](https://emdash.com/blog/what-is-emdash)

**Crystal → Nimbalyst** (Nimbalyst, MIT, 2.1k stars)
- Crystal (stravu, 3.1k stars) was "deprecated and replaced by Nimbalyst" in February 2026. [readme](https://github.com/stravu/crystal)
- Nimbalyst is an Electron app for macOS, Windows and Linux with an iOS companion; Codex, CC, OpenCode (alpha) and Copilot (alpha); worktree per session. [readme](https://github.com/Nimbalyst/nimbalyst)
- iOS users "swipe through diffs, queue the next task, and get a push when an agent is waiting"; pair by QR or pasted pairing data. [readme](https://github.com/Nimbalyst/nimbalyst) · [mobile](https://nimbalyst.com/docs/mobile/mobile-app.md)
- "Personal device sync uses end-to-end encryption with keys that stay on your devices"; hosted team data "is not zero-knowledge". An AI Usage Report compares models and tools across projects. [security](https://nimbalyst.com/docs/open-safe-private-secure/security.md) · [usage](https://nimbalyst.com/docs/session-management/ai-usage-report.md)

**Orchestrator.inc** (OrchestratorInc, Apache-2.0)
- Desktop app for macOS, Windows and Linux with a daemon and `ao` CLI; "every task gets its own agent, branch, and worktree"; 40+ agents listed. [readme](https://github.com/OrchestratorInc/agent-orchestrator)
- Failing GitHub checks go (names and links, deduplicated) to the session that owns the PR; merge conflicts ask it to rebase; unresolved review comments go to the owning worker. [ci-recovery](https://docs.orchestrator.inc/guides/ci-recovery/) · [automation](https://docs.orchestrator.inc/configuration/lifecycle-automation/)
- Reviewer agents post inline comments that are forwarded to the worker; there is a "Send to worker agent" button. [review-loop](https://docs.orchestrator.inc/guides/review-loop/)
- Mobile apps pair by QR with a one-time code (10 minutes); away from home works through `cloudflared`. "Cloudflare terminates TLS and can read tunneled traffic." [remote-access](https://docs.orchestrator.inc/configuration/remote-access/)
- Desktops on one account see each other as remote hosts; a bootstrap script for headless VMs prints a sign-in link and QR. Cron automations; "The app updates itself." [remote-access](https://docs.orchestrator.inc/configuration/remote-access/) · [readme](https://github.com/OrchestratorInc/agent-orchestrator)

**T3 Code** (pingdotgg, MIT)
- "The open-source control plane for coding agents": desktop for macOS, Windows and Linux, web, iOS, Android and CLI; CC, Codex, Cursor, OpenCode, Grok Build, Antigravity, Kiro, Pi and any ACP agent. [readme](https://github.com/pingdotgg/t3code)
- "Each thread can work in its own worktree. Review the diff, then commit, push, and open a pull request with one button." [readme](https://github.com/pingdotgg/t3code)
- Remote over LAN, Tailscale or T3 Connect (no router forwarding); QR with a `t3code://pair` link or paste the URL; optional device approval with a short code valid 10 minutes; scoped pairing tokens. [remote-access](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md)
- Up to 3 machines per free account (10 with Pro, 100 with Premier); several routes per machine; optional load balancing of new threads; "Linked environments" (beta). New threads default to Full access. [remote-access](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md) · [permissions](https://github.com/pingdotgg/t3code/blob/main/docs/user/permission-modes.md)

**Happy** (slopus, MIT, 24.8k stars)
- Desktop for macOS, Windows and Linux plus iOS, Android and web; one harness (built on Pi) for Claude, Codex, Grok, Gemini, OpenCode and any ACP agent, using your subscriptions. [readme](https://github.com/slopus/happy) · [agent](https://github.com/slopus/happy-agent)
- "everything that travels between agents, teammates, and devices is end-to-end encrypted — relays carry ciphertext they cannot read." [desktop](https://github.com/slopus/happy-desktop)
- Self-hosted relay: one Docker image with embedded Postgres and one `HANDY_MASTER_SECRET`. [self-hosting](https://happy.engineering/docs/guides/self-hosting/)
- Pairing returns "opaque `happy://` data to render as a QR code"; "Mobile messages enter the same session and permission boundary as every other client's messages". [agent](https://github.com/slopus/happy-agent)
- Permission modes from Read only to Full access, sandboxed with Seatbelt or Bubblewrap; sessions survive daemon restarts; cost is shown "as it happens". The original CLI is "in maintenance mode". [agent](https://github.com/slopus/happy-agent) · [desktop](https://github.com/slopus/happy-desktop)

**Omnara** (omnara-ai, Apache-2.0, 3.4k stars) — pivoted
- Now "the open-source managed agent platform" with its own agent loop; it is no longer a phone remote for CC. [readme](https://github.com/omnara-ai/omnara)
- "Each machine runs the `omnarad` daemon, which connects out to Omnara, so agents can run commands there without any inbound ports"; sandboxes from 12 providers. [readme](https://github.com/omnara-ai/omnara)
- Per-tool permissions, cron triggers, Slack, Discord and GitHub triggers; every step is saved in Postgres. [readme](https://github.com/omnara-ai/omnara)

**CloudCLI** (ex claude-code-webui; siteboon, AGPL, 14.7k stars)
- Self-hosted web UI for CC, Cursor CLI and Codex, a tray app for macOS, Windows and Linux, and phone access by browser; hosted cloud "Starts at €7/month". [readme](https://github.com/siteboon/claudecodeui)
- "CloudCLI UI does not include built-in authentication"; use a reverse proxy with auth, `cloudflared tunnel` for quick access, and PM2 to keep it running. [remote-server](https://cloudcli.ai/docs/installation/remote-server)
- All CC tools are "disabled by default"; community plugins add scheduling, token cost and a GitHub Issues board. [readme](https://github.com/siteboon/claudecodeui)

**Claude Code Remote Control and Managed Agents** (Anthropic)
- `claude remote-control` or `/rc`; "press spacebar to show a QR code"; sessions appear at claude.ai/code and in the Claude mobile app; "your computer has to stay on". [remote-control](https://code.claude.com/docs/en/remote-control)
- Push arrives for "permission prompts and questions"; server mode holds 32 sessions; no API keys. Traffic uses TLS via the Anthropic API and transcripts sit on Anthropic servers, so it is not described as end-to-end encrypted. [remote-control](https://code.claude.com/docs/en/remote-control)
- Managed Agents (beta): sessions in an Anthropic or self-hosted sandbox, an SSE event stream, cron via "scheduled deployments". [overview](https://platform.claude.com/docs/en/managed-agents/overview). A page named "Server-Side Sessions API" was not found (U).

**Cursor Cloud Agents** (formerly Background Agents)
- Agents "run in isolated VMs in the cloud" on separate branches; "You can run as many agents as you want in parallel." [docs](https://cursor.com/docs/background-agent)
- Start from iOS, web, desktop, Slack, a GitHub issue or PR comment, Linear or the API; results are "merge-ready PRs with artifacts". Admins can restrict outbound domains or use Tailscale. [docs](https://cursor.com/docs/background-agent)

**GitHub Copilot coding agent**
- Runs on GitHub Actions, a cloud sandbox or a custom environment; one branch and one PR per task; Actions sessions cap at 59 minutes. [about](https://docs.github.com/en/copilot/concepts/agents/coding-agent/about-coding-agent)
- Entry points include issues and "failing GitHub Actions runs"; automations run on a schedule or on events. [about](https://docs.github.com/en/copilot/concepts/agents/coding-agent/about-coding-agent)
- `@copilot` in a PR comment requests changes; Agent Merge "Watch and fix" handles failed workflows and merge conflicts. [review](https://docs.github.com/en/copilot/how-tos/agents/copilot-coding-agent/review-copilot-prs)

**OpenAI Codex (cloud)**
- "Each task has its own workspace and can keep working while your computer is asleep"; start from desktop, web, mobile or CLI; "commit or open a pull request when you're ready". [docs](https://learn.chatgpt.com/docs/cloud)
- Phone control of a local PC by QR pairing in the ChatGPT app is a third-party claim (U). [Happy alternatives](https://happy.engineering/docs/comparisons/alternatives/)

**Jules (Google) and Gemini CLI**
- Jules clones the repo "to a Cloud VM", gets plan and diff approvals, then "Jules creates a PR"; the `jules` label assigns an issue. Concurrency is 3, 15 or 60 tasks on Free, Pro or Ultra. [site](https://jules.google)
- Gemini CLI gained a read-only Plan mode in March 2026 (search snippet, U). [plan mode](https://geminicli.com/docs/cli/plan-mode)

**Amp** (Sourcegraph)
- CLI, desktop, web and mobile apps; "The apps notify you when a thread is ready for your next message." [manual](https://ampcode.com/manual)
- Orbs are "Machines that Amp creates per thread ... Send a prompt, close your laptop, and the agent keeps working."; "Forget worktrees"; agents start from Slack. [orbs](https://ampcode.com/what-are-orbs)
- `sourcegraph/amp` on GitHub returns 404.

**OpenHands "Agent Canvas"** (MIT, 94k stars)
- Self-hosted web control center for OpenHands, CC, Codex, Gemini, Pi, OpenCode "or any ACP-compatible agent", on local, Docker, VM or cloud backends. [readme](https://github.com/OpenHands/OpenHands)
- Optional Docker container per conversation; automations "on a schedule or in response to webhook events" with Slack, GitHub, Linear and Notion. [readme](https://github.com/OpenHands/OpenHands)

**Symphony (OpenAI)** — the brief listed it under mediar-ai; `mediar-ai/symphony` returns 404
- Open-source spec (Apache-2.0) plus an experimental Elixir implementation; "a low-key engineering preview for testing in trusted environments." [readme](https://github.com/openai/symphony)
- Watches an issue board, runs one isolated Codex session per issue, and delivers a PR with proof of work: "CI status, PR review feedback, complexity analysis, and walkthrough videos". [readme](https://github.com/openai/symphony)

**Terminator (mediar-ai)** — not in this category
- Windows-only computer-use SDK and MCP server that drives desktop apps through accessibility APIs. [readme](https://github.com/mediar-ai/terminator)

**Agent back-ends** (they do not orchestrate other agents)
- opencode (229k stars): CLI/TUI and desktop beta; speaks ACP. [readme](https://github.com/anomalyco/opencode)
- Cline (71.8k): IDE plugins, CLI, SDK and a native desktop app for macOS and Windows "(Tauri shell, Bun sidecar, Next.js UI)"; cron schedules; teams; checkpoints; chat-platform connectors. [readme](https://github.com/cline/cline)
- Kilo Code (28.1k): VS Code, JetBrains, CLI (an OpenCode fork), Cloud Agent and automated PR code reviews. [readme](https://github.com/Kilo-Org/kilocode)
- Goose (56.9k, Linux Foundation AAIF): desktop app and CLI; `goose acp` makes it the agent behind another app. [readme](https://github.com/aaif-goose/goose)
- Aider (50.2k, last push 2026-05-22): terminal pair programmer with auto-commits, lint/test and voice. [readme](https://github.com/Aider-AI/aider)
- Agent Client Protocol (4.7k stars): JSON-RPC between editors and agents, stable version 1, Rust crates; the site lists 48 agents and about 197 client entries. [repo](https://github.com/agentclientprotocol/agent-client-protocol) · [agents](https://agentclientprotocol.com/overview/agents) · [clients](https://agentclientprotocol.com/overview/clients)

**Newer entrants** (README level only)
- Paseo: one daemon with clients for desktop, web, iOS, Android and CLI; "each in its own worktree, on one machine or several". [readme](https://github.com/getpaseo/paseo)
- Happier: "Open source · Self-hostable · End-to-end encrypted". [readme](https://github.com/happier-dev/happier)
- Harness (autonomous-ai): a daemon per machine dials out; WebRTC direct, then Cloudflare TURN, then relay; "ChaCha20-Poly1305, X25519 session keys, pinned Ed25519 identities"; link a machine with a password. [readme](https://github.com/autonomous-ai/openharness)
- Cloudroom moves a running session between Mac and cloud ("Teleport"). Reemoat relays end-to-end encrypted messages between sessions on different machines. [Cloudroom](https://github.com/davidondrej/cloudroom) · [Reemoat](https://github.com/rends-east/reemoat)
- Pragma (Tauri, Rust, React): "Fanout — one prompt, many attempts". [readme](https://github.com/pragma-sh/pragma)
- Category list: [awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators)

## C. The 15 most commonly offered features

Counts cover the 24 rows of Section A and include only products where I read a statement. Treat them as lower bounds.

| # | Feature | Count | Products |
|---|---|---|---|
| 1 | Runs on your own machine or server | 20 | every local app and back-end; not Cursor, Codex cloud, Jules or Amp |
| 2 | Per-task isolation (worktree, branch, VM or container) | 14 | worktree: Conductor, Vibe Kanban, Claude Squad, Emdash, Nimbalyst, Orchestrator.inc, T3 Code. VM or sandbox: Cursor, Copilot, Codex, Jules, Amp, OpenHands, Symphony |
| 3 | Hosted cloud execution that outlives your laptop | 11 | Conductor (status U), Orchestrator.inc, CloudCLI, Claude Managed Agents, Omnara, Cursor, Copilot, Codex, Jules, Amp, OpenHands |
| 4 | Drives several third-party agent CLIs | 10 | Conductor, Vibe Kanban, Claude Squad, Emdash, Nimbalyst, Orchestrator.inc, T3 Code, Happy, CloudCLI, OpenHands |
| 5 | Phone access | 10 | Conductor, Nimbalyst, Orchestrator.inc, T3 Code, Happy, CloudCLI, Claude Code, Cursor, Codex, Amp |
| 6 | Diff review inside the app | 9 | Conductor, Vibe Kanban, Claude Squad, Emdash, Nimbalyst, Orchestrator.inc, T3 Code, Happy, Jules |
| 7 | Merge from the app or merge automation | 9 | Conductor, Vibe Kanban, Emdash, Nimbalyst, Orchestrator.inc, T3 Code, Cursor, Copilot, Jules |
| 8 | Scheduling (cron or event automations) | 9 | Conductor, Emdash, Orchestrator.inc, T3 Code, Cline, Omnara, Claude Managed Agents, Copilot, OpenHands |
| 9 | Issue → agent → pull request | 8 | Conductor, Vibe Kanban, Emdash, Orchestrator.inc, Cursor, Copilot, Jules, Symphony |
| 10 | Built-in relay or tunnel for remote access | 6 | Conductor, Nimbalyst, Orchestrator.inc, T3 Code, Happy, Claude Code |
| 11 | QR or one-time-code pairing | 6 | Conductor, Nimbalyst, Orchestrator.inc, T3 Code, Happy, Claude Code |
| 12 | PR review comments routed back to the agent | 5 | Conductor, Vibe Kanban, Orchestrator.inc, Cursor, Copilot |
| 13 | Approvals answered from a phone | 5 | Nimbalyst, Orchestrator.inc, T3 Code, Happy, Claude Code |
| 14 | Push notification when an agent needs you | 5 | Nimbalyst, Orchestrator.inc, Happy, Claude Code, Amp |
| 15 | In-app preview browser or dev server | 5 | Conductor, Vibe Kanban, Orchestrator.inc, Happy, CloudCLI |

Rare: control of several of your own machines (4: Emdash, Orchestrator.inc, T3 Code, Omnara); usage and cost dashboards (4: Conductor, Nimbalyst, Happy, Emdash); end-to-end encrypted remote traffic (3: Conductor, Happy, Nimbalyst); checkpoints or rewind (2: Conductor, Cline); CI failure routed automatically to the agent (2: Orchestrator.inc, Copilot); N attempts per prompt with best-of-N (none in the matrix; Pragma only).

## D. UX patterns worth copying

- **Pair with a QR that carries the relay address and a one-time secret**, single use, expiring after 10 minutes. [Conductor](https://www.conductor.build/docs/reference/remote-control) · [Orchestrator.inc](https://docs.orchestrator.inc/configuration/remote-access/) · [T3 Code](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md) · [Happy](https://github.com/slopus/happy-agent)
- **Keep the secret out of server logs.** Orchestrator.inc carries the credential "in its fragment" and checks the host identity before presenting it. [remote-access](https://docs.orchestrator.inc/configuration/remote-access/)
- **Offer a paste fallback next to the QR.** Nimbalyst has "Copy to Clipboard"; Orchestrator.inc has "Enter details manually". [Nimbalyst](https://nimbalyst.com/docs/mobile/mobile-app.md) · [Orchestrator.inc](https://docs.orchestrator.inc/configuration/remote-access/)
- **Confirm a short code on a second device.** T3's headless setup prints a link and a code to match, and a new device shows a code such as `K7QM-2XFD` that an existing device approves. [T3 Code](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md)
- **One command to enrol a headless machine**: a curl script installs the service, then prints a sign-in link and QR. [Orchestrator.inc](https://docs.orchestrator.inc/configuration/remote-access/) · [Harness](https://github.com/autonomous-ai/openharness)
- **One click to send a review thread back to the agent.** Conductor has "Send unresolved comments to the agent"; Orchestrator.inc has "Send to worker agent". [Conductor](https://www.conductor.build/docs/reference/checks) · [Orchestrator.inc](https://docs.orchestrator.inc/guides/review-loop/)
- **Route CI failures to the owning session only when the failure set changes**, sending names and links rather than raw logs. [Orchestrator.inc](https://docs.orchestrator.inc/guides/ci-recovery/)
- **One merge-readiness panel** listing git status, PR, CI, deployments, review threads and todos, able to block merge. [Conductor](https://www.conductor.build/docs/reference/checks)
- **Revert to any earlier turn from the chat**, backed by a private Git ref per turn. [Conductor](https://www.conductor.build/docs/reference/checkpoints)
- **A machine card that shows health and drift**: connection, resource usage and a runtime update button. [Emdash](https://emdash.com/docs/remote-development/machines)
- **A usage page by model and workspace over 7, 30 and 90 days**, plus context fill and estimated cost behind a circle in the composer. [Conductor](https://www.conductor.build/changelog/0.96.0-know-what-you-re-spending)
- **Say plainly what the relay can read.** Orchestrator.inc states "Cloudflare terminates TLS and can read tunneled traffic" and tells users to test with the phone off the home Wi-Fi. [remote-access](https://docs.orchestrator.inc/configuration/remote-access/)
- **Scope what a paired device may do.** T3 issues tokens limited to scopes such as `orchestration:read`; Orchestrator.inc's "Revoke all devices" also rotates the password. [T3 Code](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md) · [Orchestrator.inc](https://docs.orchestrator.inc/configuration/remote-access/)
- **Phone review built for thumbs**: "swipe through diffs", "queue the next task", reply by voice. [Nimbalyst](https://github.com/Nimbalyst/nimbalyst)
- **Keep the host awake on purpose.** Orchestrator.inc's "Keep this Mac awake" includes closed-lid running, with battery and thermal stops. [remote-access](https://docs.orchestrator.inc/configuration/remote-access/)
- **First-run setup checks.** Conductor verifies `gh auth status` and each agent login and walks through anything missing. [Conductor](https://www.conductor.build/docs/installation)

## E. Gaps a cross-machine, multi-CLI desktop app could own

1. **End-to-end encrypted self-hosted relay, several own machines, several CLIs, and a PR pipeline in one product.** No product was verified to do all of these. Conductor encrypts but is Mac-only on a vendor relay. Happy encrypts and self-hosts but has no worktree or PR pipeline. Orchestrator.inc and T3 Code span machines but ride Cloudflare. Emdash spans machines over SSH only.
2. **Closing the GitHub loop.** Only Orchestrator.inc and Copilot route CI failures to the agent automatically, and only five route review comments. Nobody I read verifies a deploy step (Vercel or Cloudflare) after merge; Conductor only lists deployments.
3. **N attempts per task with side-by-side comparison.** Pragma is the only example found; the leaders make you open workspaces by hand.
4. **Cost and usage across machines and providers.** Four products show anything, mostly for one machine. A fleet view by machine, agent, project and day is open.
5. **Machine health and agent version drift.** Emdash shows one machine's runtime status; nobody shows a fleet table of CLI versions, login expiry or pending updates.
6. **Reach without a vendor account.** T3 allows 3 machines free; Orchestrator.inc's fixed address needs an account, and its own docs say raw Tailscale endpoints fail on iOS.
7. **Windows and Linux parity.** Conductor is Mac-only; Cline's desktop app is Mac and Windows only.
8. **Surviving vendor churn.** Vibe Kanban's company closed, Crystal was deprecated, Omnara pivoted, Conductor announced a wind-down and reversed nine days later, and Emdash is moving to a cloud product. An open data format and one-click export are a feature here.

## What users say (search-result summaries; threads not read)

- Hacker News users credit Conductor with popularising worktree isolation and say Emdash, Conductor, Vibe Kanban and T3 Code are "converging on the same ideas".
- A Reddit user on r/ClaudeCode "loved VibeKanban at first" but is leaving because it is sunsetting; another keeps returning to plain Claude Code for "less moving parts and fewer weird edge cases".
- Happy's original Android app drew a feature-parity request (GitHub issue #605).

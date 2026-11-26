// 界面测试共用的模拟后端：在页面里冒充 Tauri 与 Rust 端。
// 每个命令在这里有一个处理函数；测试通过 window.__mock 改状态、推送事件、查看收到的调用。
//
// 这个函数会被序列化后注入页面，不能引用模块作用域里的任何东西。

// 注入页面的模拟实现：只实现会话页用到的命令
const MOCK = () => {
  let nextId = 1;
  const callbacks = new Map();
  const listeners = new Map();
  const state = {
    nextSession: 1,
    nextMessage: 1000,
    sessions: [],
    messages: new Map(),
    live: [],
    calls: [],
    remembered: [],
    /** 机器：本机 + 远程 */
    machines: [],
    /** 远程机器上的 Agent：nodeId → 清单 */
    remoteAgents: new Map(),
    /** 中继设置与状态 */
    relay: null,
    /** 活动记录 */
    events: [],
    /** 任务流水线 */
    tasks: [],
    /** 本机桌面 Agent 的运行状态（desktop_states 用） */
    desktop: [{ path: "/Applications/ZCode.app", running: false }],
    taskDiffs: {},
    taskPatches: {},
    /** 为 true 时，下一次刷新把审查中的任务标成已合并 */
    mergePr: false,
    cloneCalls: [],
    ghUser: null,
    /** 部署：两家平台的连接、登记过的项目、部署记录 */
    deploy: {
      platforms: {
        vercel: { configured: true, error: null, who: "octo", scopes: [{ id: "team_a", name: "Acme" }], scope: "team_a" },
        cloudflare: { configured: false, error: null, who: null, scopes: [], scope: null },
      },
      targets: [],
      deployments: [],
      nextId: 10,
      unhealthy: [],
    },
    /** 工作流编排 */
    workflows: [],
    wfTemplates: [],
    /** 自动化规则 */
    rules: [],
    ruleRuns: [],
  };
  let nextTask = 4;
  let nextSessionId = 60;
  const now = "2026-11-24 03:00:00";
  const makeTask = (fields) => ({
    id: nextTask++,
    title: "",
    source: "issue",
    repo: "octo/demo",
    issueNumber: null,
    issueUrl: null,
    agentId: null,
    status: "pending",
    branch: null,
    createdAt: now,
    updatedAt: now,
    nodeId: null,
    adapter: null,
    model: null,
    workdir: null,
    worktree: null,
    baseBranch: null,
    baseSha: null,
    headSha: null,
    sessionId: null,
    prNumber: null,
    prUrl: null,
    detail: null,
    failure: null,
    instructions: null,
    autoPr: false,
    checks: null,
    ...fields,
  });
  state.tasks.push(
    makeTask({
      id: 1, title: "修复登录态过期后白屏", issueNumber: 7, issueUrl: "https://github.com/octo/demo/issues/7", branch: "agent/issue-7",
    }),
    makeTask({
      id: 2, title: "给部署页补测试", issueNumber: 3, issueUrl: "https://github.com/octo/demo/issues/3", branch: "agent/issue-3",
      status: "reviewing", nodeId: "local", adapter: "claude-cli", model: "sonnet", workdir: "/Users/tester/code/demo",
      worktree: "/Users/tester/code/demo-wt/agent-issue-3", baseBranch: "main", baseSha: "aaa", headSha: "bbb", sessionId: 41,
      prNumber: 101, prUrl: "https://github.com/octo/demo/pull/101",
      detail: "PR #101 等审查：1 项通过，0 项进行中，1 项失败",
      checks: [
        { name: "build", state: "failure", url: "https://github.com/octo/demo/actions/runs/1" },
        { name: "lint", state: "success", url: "https://github.com/octo/demo/actions/runs/2" },
      ],
    }),
    makeTask({
      id: 3, title: "升级依赖", source: "manual", status: "failed", instructions: "把依赖都升到最新", branch: "agent/task-3",
      nodeId: "local", adapter: "claude-cli", workdir: "/Users/tester/code/demo", worktree: "/Users/tester/code/demo-wt/agent-task-3",
      baseBranch: "main", sessionId: 42, detail: "出错了，可以重试", failure: "这一轮没有完成：这台机器上同时运行的对话已达上限（8）",
    }),
  );
  nextTask = 4;
  // 部署：一个 Vercel 项目、一个 Cloudflare Pages 项目
  const target = (fields) => ({
    id: 0, provider: "vercel", name: "", platformId: "", scope: "team_a", url: null, repo: null, repoId: null, gitType: "github",
    branch: "main", framework: null, prodRequiresApproval: true, autoFix: true, autoPr: false, autoDeploy: false, autoRollback: false, healthPath: "/", healthContains: null,
    hasHook: false, createdAt: now, updatedAt: now, ...fields,
  });
  state.deploy.targets.push(
    target({ id: 1, name: "web", platformId: "prj_web", url: "https://demo.example.com", repo: "octo/demo", framework: "nextjs" }),
    target({
      id: 2, provider: "cloudflare-pages", name: "site", platformId: "site", scope: "acct1", url: "https://site.example.com", repo: "octo/demo",
    }),
  );
  const ms = Date.parse("2026-11-24T02:00:00Z");
  const dep = (fields) => ({
    id: "", targetId: 1, provider: "vercel", project: "web", env: "preview", state: "ready", rawState: "READY", url: null,
    inspectorUrl: "https://vercel.com/acme/web/abc", branch: "main", commitSha: null, commitMessage: null, author: "Octo",
    createdMs: ms, buildingMs: ms, readyMs: ms + 90_000, errorCode: null, errorMessage: null, rollbackOk: false, current: false,
    guarded: true, health: null, taskId: null, ...fields,
  });
  state.deploy.deployments.push(
    dep({ id: "dpl_cur", env: "production", commitSha: "abcdef123456", commitMessage: "ship it", current: true, rollbackOk: true, health: "ok" }),
    dep({ id: "dpl_fail", env: "production", state: "error", rawState: "ERROR", commitSha: "1111111222", commitMessage: "break the build", errorCode: "BUILD_FAILED", errorMessage: 'Command "npm run build" exited with 1', taskId: 3 }),
    dep({ id: "dpl_build", state: "building", rawState: "BUILDING", branch: "feature/x", commitSha: "fffff0000", commitMessage: "work in progress", createdMs: ms + 30_000, readyMs: null }),
    dep({ id: "dpl_preview", branch: "agent/issue-3", commitSha: "abcdef123456", commitMessage: "给部署页补测试", url: "https://web-git-agent-issue-3.vercel.app", createdMs: ms - 60_000 }),
    dep({ id: "dpl_prev", env: "production", commitSha: "aaaaaa0000", commitMessage: "previous release", rollbackOk: true, createdMs: ms - 3_600_000 }),
    dep({ id: "pg_ok", targetId: 2, provider: "cloudflare-pages", project: "site", env: "production", commitSha: "cccccc3333", commitMessage: "ship pages", current: true, health: "ok", inspectorUrl: null }),
  );
  // 平台上还没登记的项目
  const candidates = [
    { provider: "vercel", name: "web", platformId: "prj_web", url: null, repo: "octo/demo", repoId: "1234", branch: "main", framework: "nextjs" },
    { provider: "vercel", name: "api", platformId: "prj_api", url: null, repo: "octo/api", repoId: "5678", branch: "main", framework: null },
    { provider: "cloudflare-pages", name: "site", platformId: "site", url: "https://site.example.com", repo: "octo/demo", repoId: "1234", branch: "main", framework: "nextjs" },
    { provider: "cloudflare-pages", name: "docs", platformId: "docs", url: "https://docs.pages.dev", repo: "octo/docs", repoId: "9", branch: "main", framework: null },
    { provider: "cloudflare-workers", name: "api-worker", platformId: "tag_api_worker", url: null, repo: null, repoId: null, branch: null, framework: null },
  ];
  const findTarget = (id) => {
    const t = state.deploy.targets.find((x) => x.id === id);
    if (!t) throw new Error(`部署目标不存在：#${id}`);
    return t;
  };
  const findDep = (id) => {
    const d = state.deploy.deployments.find((x) => x.id === id);
    if (!d) throw new Error(`部署记录不存在：${id}`);
    return d;
  };
  const machineInfo = (name, home) => ({
    name, hostname: name, os: "macos", arch: "arm64", kernel: "25.5.0", version: "0.2.0",
    flavor: "桌面应用内置", home, uptimeS: 3600, cpuCount: 12, memTotal: 16 * 1024 ** 3,
  });
  state.machines.push({
    id: "local", name: "本机", local: true, online: true,
    info: machineInfo("本机", "/Users/tester"), status: null, agentCount: 4,
  });
  state.relay = {
    configured: false, addr: "", fingerprint: "", hasToken: false,
    installScript: "https://raw.githubusercontent.com/1989tianlong/AutoAgents/main/scripts/install-agentd.sh",
    connected: false, message: null, online: [], pending: [],
  };
  // 工作流模板：与后端 workflow.rs 里的 TEMPLATES 一一对应
  state.nextWorkflow = 1;
  state.wfFailNext = [];
  state.wfTemplates = [
    {
      id: "issue-fix", name: "修 Issue 流水线", desc: "一个 Agent 分析修复，第二个交叉审查，第三个撰写变更说明",
      needsPr: false, live: null,
      steps: [
        { name: "分析修复", kind: "task", deps: [], adapter: "claude-cli" },
        { name: "交叉审查", kind: "session", deps: ["1"], adapter: "codex-cli" },
        { name: "撰写变更说明", kind: "session", deps: ["2"], adapter: "grok-cli" },
      ],
    },
    {
      id: "pr-review", name: "PR 审查流水线", desc: "两位 Agent 并行交叉审查同一个 PR，第三人汇总结论",
      needsPr: true, live: null,
      steps: [
        { name: "审查 · Agent 一", kind: "session", deps: [], adapter: "claude-cli" },
        { name: "审查 · Agent 二", kind: "session", deps: [], adapter: "codex-cli" },
        { name: "汇总结论", kind: "session", deps: ["1", "2"], adapter: null },
      ],
    },
    {
      id: "race", name: "竞争模式", desc: "两个 Agent 并行解决同一 Issue（各自分支），人工评审择优",
      needsPr: false, live: null,
      steps: [
        { name: "方案 A", kind: "task", deps: [], adapter: "claude-cli" },
        { name: "方案 B", kind: "task", deps: [], adapter: "codex-cli" },
        { name: "人工评审", kind: "gate", deps: ["?1", "?2"], adapter: null },
      ],
    },
    {
      id: "publish-guard", name: "发布守护流水线", desc: "生产部署 → 健康检查 → 失败自动建修复任务",
      needsPr: false, live: "/deploy", steps: [],
    },
  ];
  const agents = ["claude-cli", "codex-cli", "grok-cli", "pi"].map((adapter, i) => ({
    id: `local/${adapter}`,
    name: ["Claude CLI", "Codex CLI", "Grok CLI", "Pi Agent"][i],
    kind: "cli",
    level: "L3",
    adapter,
    path: `/bin/${adapter}`,
    version: "1.0",
  }));
  agents.push({
    id: "local/zcode", name: "ZCode", kind: "desktop", level: "L1", adapter: "zcode",
    path: "/Applications/ZCode.app", version: null,
  });
  const models = {
    "claude-cli": {
      adapter: "claude-cli",
      source: "~/.claude/settings.json",
      default: "sonnet[1m]",
      options: [
        { value: "auto/glm-5.3-flash[1m]", label: "GLM-5.3-Flash · routing group", detail: "via magpie" },
        { value: "zai/glm-5.3[1m]", label: "GLM-5.3 · Z.AI Coding Plan", detail: "via magpie" },
      ],
      custom: [],
    },
    "grok-cli": {
      adapter: "grok-cli",
      source: "~/.grok/config.toml",
      default: "magpie/grok/grok-4.7",
      options: [
        { value: "magpie/grok/grok-4.7", label: "Grok 4.7 · Grok", detail: null },
        { value: "magpie/auto/glm-5.3-flash", label: "GLM-5.3-Flash · routing group", detail: null },
        { value: "magpie/zai/glm-5.3", label: "GLM-5.3 · Z.AI", detail: null },
      ],
      custom: [],
    },
    pi: { adapter: "pi", source: "pi --list-models", default: "magpie/grok/grok-4.7", options: [], custom: [] },
  };
  const handlers = {
    "plugin:event|listen": ({ event, handler }) => {
      listeners.set(handler, { event, id: handler });
      return handler;
    },
    "plugin:event|unlisten": ({ eventId }) => {
      listeners.delete(eventId);
      return null;
    },
    app_info: () => ({ name: "autoagents", version: "0.2.0", os: "macos", arch: "aarch64", hostname: "test", home: "/Users/tester" }),
    discover_agents: () => agents,
    // ---------- 机器 ----------
    agents_all: () =>
      state.machines.flatMap((m) =>
        (m.local ? agents : state.remoteAgents.get(m.id) ?? []).map((a) => ({
          ...a, nodeId: m.id, nodeName: m.name, online: m.online,
        })),
      ),
    machines_list: () => state.machines,
    nodes_list: () => state.machines.filter((m) => !m.local),
    relay_get: () => state.relay,
    relay_status: () => ({
      relay: state.relay,
      connected: state.relay.connected,
      message: state.relay.message,
      online: state.relay.online,
      pending: state.relay.pending,
    }),
    relay_set: ({ text, addr, token }) => {
      const query = String(text ?? "").split("?")[1] ?? "";
      const field = (key) => query.split("&").find((p) => p.startsWith(`${key}=`))?.slice(key.length + 1);
      const address = field("addr") ?? addr;
      if (!address) throw new Error("mock: 缺少中继地址");
      Object.assign(state.relay, {
        configured: true, addr: address, hasToken: true,
        fingerprint: field("fp") ?? "sha256:0011223344556677889900112233445566778899001122334455667788990011",
        token: field("token") ?? token, connected: true,
      });
      return state.relay;
    },
    relay_clear: () => {
      Object.assign(state.relay, { configured: false, addr: "", fingerprint: "", hasToken: false, connected: false, online: [], pending: [] });
      return null;
    },
    install_script_set: ({ url }) => {
      state.relay.installScript = url;
      return null;
    },
    node_install_command: ({ name, allow }) => {
      if (!state.relay.configured) throw new Error("还没有设置中继：到「机器」页粘贴中继的连接串");
      const code = "ABCD-EFGH-JKLM-NPQR";
      state.relay.pending = state.relay.pending.filter((p) => p.name !== name);
      state.relay.pending.push({ name, expiresInS: 600 });
      state.codes = state.codes ?? {};
      state.codes[name] = code;
      const command = `curl -fsSL '${state.relay.installScript}' | sh -s -- install --name '${name}' --relay '${state.relay.addr}' --token '${state.relay.token}' --pair ${code}`
        + (allow.length ? ` ${allow.map((dir) => `--allow '${dir}'`).join(" ")}` : "");
      return { name, code, command, expiresInS: 600 };
    },
    node_pair: ({ nodeId, code, name }) => {
      if (code !== "WXYZ-WXYZ-WXYZ-WXYZ") throw new Error("与「name」握手失败：节点拒绝了连接：这台机器还没有和该控制端配对");
      window.__mock.addMachine(nodeId, name ?? nodeId, { online: true });
      state.relay.online = state.relay.online.map((n) => (n.nodeId === nodeId ? { ...n, paired: true } : n));
      return state.machines.find((m) => m.id === nodeId);
    },
    node_pair_direct: () => {
      throw new Error("mock: 未实现直连配对");
    },
    node_authorize: ({ nodeId }) => {
      if (!state.machines.find((m) => m.id === nodeId)?.online) throw new Error("机器不在线");
      return "AUTH-CODE-1234-5678";
    },
    node_remove: ({ nodeId }) => {
      state.machines = state.machines.filter((m) => m.id !== nodeId);
      // 移除只是控制端这边忘了它；它在中继上还在线，只是不再算已配对
      state.relay.online = state.relay.online.map((n) => (n.nodeId === nodeId ? { ...n, paired: false } : n));
      return null;
    },
    node_rename: ({ nodeId, name }) => {
      const machine = state.machines.find((m) => m.id === nodeId);
      if (!machine) throw new Error(`没有添加过机器「${nodeId}」`);
      machine.name = name;
      state.relay.online = state.relay.online.map((n) => (n.nodeId === nodeId ? { ...n, name } : n));
      return machine;
    },
    node_connect_now: () => null,
    node_metrics: ({ nodeId }) => ({
      cpuPercent: 23.5, memUsed: 8 * 1024 ** 3, memTotal: 16 * 1024 ** 3,
      load1: 1.2, load5: 1.1, load15: 0.9, uptimeS: 100000, runningTurns: nodeId === "local" ? 0 : 1,
    }),
    node_ls: ({ path }) => {
      if (!path.startsWith("/")) throw new Error(`无法访问 ${path}：No such file or directory`);
      const parent = path.replace(/\/+$/, "").split("/").slice(0, -1).join("/") || "/";
      return {
        path, parent: path === "/" ? null : parent,
        entries: [
          { name: "code", git: true },
          { name: "Documents", git: false },
        ],
      };
    },
    node_git: ({ args }) => ({ code: 0, stdout: `git ${args.join(" ")}\n`, stderr: "" }),
    // ---------- 活动与用量 ----------
    events_list: ({ limit = 100, kind = null }) =>
      state.events.filter((e) => !kind || e.kind === kind || e.kind.startsWith(`${kind}.`)).slice(0, limit),
    events_counts: () => {
      const counts = { total: state.events.length, warning: 0, error: 0, byKind: {} };
      for (const e of state.events) {
        if (e.level === "warning") counts.warning++;
        if (e.level === "error") counts.error++;
        const module = e.kind.split(".")[0];
        counts.byKind[module] = (counts.byKind[module] ?? 0) + 1;
      }
      return counts;
    },
    events_clear: () => {
      const removed = state.events.length;
      state.events = [];
      return removed;
    },
    usage_summary: () => ({
      turns: 3, costUsd: 0.09, inputTokens: 300, outputTokens: 15, avgDurationMs: 4200,
      last24hCostUsd: 0.05, last24hTurns: 2,
      byDay: [
        { day: "2026-11-23", turns: 1, costUsd: 0.04, tokens: 100 },
        { day: "2026-11-24", turns: 2, costUsd: 0.05, tokens: 215 },
      ],
      byAgent: [{ name: "claude-cli", turns: 3, costUsd: 0.09, inputTokens: 300, outputTokens: 15 }],
      byMachine: [{ name: "本机", turns: 3, costUsd: 0.09, inputTokens: 300, outputTokens: 15 }],
      topSessions: [{ name: "hello", turns: 3, costUsd: 0.09, inputTokens: 300, outputTokens: 15 }],
    }),
    github_status: () => state.ghUser,
    github_connect: () => {
      state.ghUser = { login: "octo", name: "Octo Cat", avatarUrl: null };
      return state.ghUser;
    },
    github_disconnect: () => {
      state.ghUser = null;
      return null;
    },
    github_repos: () => [
      {
        id: 1, fullName: "octo/demo", private: false, description: "演示仓库", openIssues: 2, updatedAt: null,
        htmlUrl: "https://github.com/octo/demo", defaultBranch: "main", sshUrl: "git@github.com:octo/demo.git",
      },
    ],
    github_issues: () => [
      {
        number: 9, title: "首页加载太慢", body: "首屏要 5 秒。", state: "open", author: "octo", labels: ["performance"],
        updatedAt: null, htmlUrl: "https://github.com/octo/demo/issues/9",
      },
    ],
    github_prs: () => [],
    tasks_list: () => state.tasks,
    task_get: ({ id }) => {
      const task = state.tasks.find((t) => t.id === id);
      if (!task) throw new Error(`任务不存在：#${id}`);
      return task;
    },
    task_create_issue: ({ title, repo, issueNumber, issueUrl }) => {
      const task = makeTask({ title, repo, issueNumber, issueUrl, branch: `agent/issue-${issueNumber}` });
      state.tasks.unshift(task);
      return task;
    },
    task_create_manual: ({ title, repo, instructions }) => {
      const task = makeTask({ title, repo, instructions, source: "manual" });
      task.branch = `agent/task-${task.id}`;
      state.tasks.unshift(task);
      return task;
    },
    task_set_status: ({ id, status }) => {
      Object.assign(state.tasks.find((t) => t.id === id), { status });
      return null;
    },
    task_delete: ({ id }) => {
      state.tasks = state.tasks.filter((t) => t.id !== id);
      return null;
    },
    task_start: ({ id, nodeId, adapter, model, workdir, baseBranch, autoPr }) => {
      const task = state.tasks.find((t) => t.id === id);
      if (!workdir.includes("demo")) throw new Error("确认仓库位置失败：fatal: not a git repository (or any of the parent directories): .git");
      const sessionId = nextSessionId++;
      Object.assign(task, {
        status: "running", nodeId, adapter, model, workdir, autoPr,
        worktree: `${workdir}-wt/${task.branch.replace("/", "-")}`,
        baseBranch: baseBranch ?? "main", baseSha: "aaa", headSha: null, prNumber: null, prUrl: null, checks: null,
        failure: null, detail: "Agent 正在处理", sessionId,
      });
      // 流水线开的会话：会话页能链回任务
      state.sessions.unshift({
        id: sessionId, adapter, title: task.title, model, workdir: task.worktree, nodeId, taskId: task.id, createdAt: now,
      });
      state.messages.set(sessionId, []);
      return task;
    },
    task_submit: ({ id }) => {
      const task = state.tasks.find((t) => t.id === id);
      const number = 100 + id;
      Object.assign(task, {
        status: "reviewing", prNumber: number, prUrl: `https://github.com/octo/demo/pull/${number}`, headSha: "bbb",
        detail: `PR #${number} 等审查：1 项通过，1 项进行中，1 项失败`,
        checks: [
          { name: "build", state: "failure", url: "https://github.com/octo/demo/actions/runs/1" },
          { name: "lint", state: "success", url: null },
          { name: "test", state: "pending", url: null },
        ],
      });
      delete state.taskDiffs[id];
      return task;
    },
    task_fix: ({ id }) => {
      Object.assign(state.tasks.find((t) => t.id === id), { status: "running", detail: "Agent 正在修复检查指出的问题" });
      return null;
    },
    task_refresh: ({ id }) => {
      const task = state.tasks.find((t) => t.id === id);
      if (state.mergePr && task.status === "reviewing") {
        Object.assign(task, { status: "done", detail: `PR #${task.prNumber} 已合并` });
      }
      return task;
    },
    tasks_refresh: () => 0,
    task_abort: ({ id }) => {
      Object.assign(state.tasks.find((t) => t.id === id), { status: "failed", detail: "出错了，可以重试", failure: "这一轮没有完成：已停止" });
      return null;
    },
    task_diff: ({ id }) => {
      const task = state.tasks.find((t) => t.id === id);
      return state.taskDiffs[id] ?? { worktree: task.worktree ?? "", branch: task.branch, baseBranch: task.baseBranch, ahead: 0, stat: "", entries: [] };
    },
    task_diff_file: ({ path }) => state.taskPatches[path] ?? "",
    repo_clone: ({ nodeId, url, dir }) => {
      state.cloneCalls.push({ nodeId, url, dir });
      return dir;
    },
    // ---------- 部署 ----------
    deploy_status: () => state.deploy.platforms,
    deploy_connect: ({ platform, token }) => {
      if (platform !== "vercel" && platform !== "cloudflare") throw new Error(`不认识的平台：${platform}`);
      if (!token.trim()) throw new Error("请输入 API 令牌");
      const good = platform === "vercel" ? "vcp_good" : "cf_good";
      if (token.trim() !== good) {
        throw new Error(`${platform === "vercel" ? "Vercel" : "Cloudflare"} API 错误（403）：Not authorized，令牌无效或已过期`);
      }
      const scopes = platform === "vercel"
        ? [{ id: "team_a", name: "Acme" }, { id: "team_b", name: "Beta" }]
        : [{ id: "acct1", name: "Acme" }, { id: "acct2", name: "Beta" }];
      state.deploy.platforms[platform] = { configured: true, error: null, who: platform === "vercel" ? "octo" : null, scopes, scope: scopes[0].id };
      return state.deploy.platforms[platform];
    },
    deploy_disconnect: ({ platform }) => {
      state.deploy.platforms[platform] = { configured: false, error: null, who: null, scopes: [], scope: null };
      return null;
    },
    deploy_set_scope: ({ platform, scope }) => {
      state.deploy.platforms[platform].scope = scope;
      return null;
    },
    deploy_discover: ({ platform }) => {
      const s = state.deploy.platforms[platform];
      if (!s.configured) throw new Error(`尚未连接 ${platform === "vercel" ? "Vercel" : "Cloudflare"}：请先填入 API 令牌`);
      const scope = platform === "vercel" ? "team_a" : "acct1";
      return candidates
        .filter((c) => (platform === "vercel" ? c.provider === "vercel" : c.provider.startsWith("cloudflare")))
        .map((c) => ({ ...c, registered: state.deploy.targets.find((t) => t.provider === c.provider && t.platformId === c.platformId)?.id ?? null, scope }));
    },
    deploy_targets: () => state.deploy.targets,
    deploy_target_add: ({ provider, name, platformId, url, hookUrl }) => {
      const c = candidates.find((x) => x.provider === provider && x.platformId === platformId);
      const t = target({
        id: state.deploy.nextId++, provider, name, platformId, scope: provider === "vercel" ? "team_a" : "acct1",
        url: url || c?.url || null, repo: c?.repo ?? null, repoId: c?.repoId ?? null, gitType: c?.repo ? "github" : null,
        branch: c?.branch ?? null, framework: c?.framework ?? null, hasHook: !!hookUrl,
      });
      state.deploy.targets.push(t);
      return t;
    },
    deploy_target_update: ({ id, hookUrl, healthContains, ...patch }) => {
      const t = findTarget(id);
      if (hookUrl !== undefined && hookUrl !== null) {
        if (hookUrl !== "" && !hookUrl.startsWith("https://")) throw new Error("Deploy Hook 的地址应该以 https:// 开头");
        t.hasHook = hookUrl !== "";
      }
      Object.assign(t, patch, healthContains === "" ? { healthContains: null } : {});
      return t;
    },
    deploy_target_remove: ({ id }) => {
      findTarget(id);
      state.deploy.targets = state.deploy.targets.filter((t) => t.id !== id);
      state.deploy.deployments = state.deploy.deployments.filter((d) => d.targetId !== id);
      return null;
    },
    deploy_list: ({ targetId, limit }) =>
      state.deploy.deployments.filter((d) => targetId == null || d.targetId === targetId).slice(0, limit ?? 50),
    deploy_refresh: () => 0,
    /** 发起一次部署；生产分支（Workers 一律算生产）要确认 */
    deploy_trigger: ({ targetId, branch, commit, confirmed }) => {
      const t = findTarget(targetId);
      const workers = t.provider === "cloudflare-workers";
      if (workers && !t.hasHook) throw new Error("这个 Worker 没有配 Deploy Hook：在 Cloudflare 控制台「Settings → Builds → Deploy Hooks」里建一个，填到目标设置里");
      const production = workers || branch === t.branch;
      if (production && t.prodRequiresApproval && !confirmed) throw new Error("这是生产部署，需要先确认");
      const fresh = dep({
        id: `dpl_new${state.deploy.nextId++}`, targetId, provider: t.provider, project: t.name, env: production ? "production" : "preview",
        state: "queued", rawState: "QUEUED", branch: workers ? t.branch : branch, commitSha: commit, commitMessage: "triggered", readyMs: null,
      });
      state.deploy.deployments.unshift(fresh);
      return fresh;
    },
    deploy_retry: ({ id, confirmed }) => {
      const d = findDep(id);
      const t = findTarget(d.targetId);
      if (d.env === "production" && t.prodRequiresApproval && !confirmed) throw new Error("这是生产部署，需要先确认");
      const fresh = { ...d, id: `dpl_again${state.deploy.nextId++}`, state: "queued", rawState: "QUEUED", current: false, health: null, taskId: null };
      state.deploy.deployments.unshift(fresh);
      return fresh;
    },
    deploy_cancel: ({ id }) => {
      Object.assign(findDep(id), { state: "canceled", rawState: "CANCELED" });
      return null;
    },
    deploy_promote: ({ id }) => {
      const d = findDep(id);
      state.deploy.deployments.forEach((x) => { if (x.targetId === d.targetId) x.current = false; });
      Object.assign(d, { env: "production", current: true });
      return null;
    },
    deploy_rollback: ({ targetId, toId, versionId }) => {
      findTarget(targetId);
      const d = findDep(toId);
      state.deploy.deployments.forEach((x) => { if (x.targetId === targetId) x.current = false; });
      d.current = true;
      d.rawState = versionId ? "ROLLED_BACK_TO_VERSION" : "ROLLED_BACK";
      return null;
    },
    deploy_history: ({ targetId }) =>
      state.deploy.deployments
        .filter((d) => d.targetId === targetId && d.env === "production" && d.state === "ready")
        .map((d) => ({ id: d.id, createdMs: d.createdMs, branch: d.branch, commitMessage: d.commitMessage, current: d.current, versionId: d.id === "wk_v" ? "ver1" : null })),
    deploy_logs: ({ id }) => {
      const d = findDep(id);
      return `--- 构建 ${d.id} ---\nRunning "npm run build"\n${d.state === "error" ? "Type error: Cannot find name 'useEffect'" : "Compiled successfully"}`;
    },
    /** 复查健康：unhealthy 里列着的部署一律不通过 */
    deploy_check_health: ({ id }) => {
      const text = state.deploy.unhealthy.includes(id) ? "503 Service Unavailable" : "ok";
      Object.assign(findDep(id), { health: text });
      return text;
    },
    deploy_for_task: ({ taskId }) => {
      const task = state.tasks.find((t) => t.id === taskId);
      if (!task) throw new Error(`任务不存在：#${taskId}`);
      return state.deploy.deployments.filter(
        (d) => d.taskId === taskId || d.branch === task.branch || (task.headSha && d.commitSha === task.headSha),
      );
    },
    // ---------- 工作流编排 ----------
    workflow_templates: () => state.wfTemplates,
    workflow_list: () => state.workflows,
    workflow_get: ({ id }) => {
      const wf = state.workflows.find((w) => w.id === id);
      if (!wf) throw new Error(`工作流不存在：#${id}`);
      return wf;
    },
    workflow_create: (args) => {
      const t = state.wfTemplates.find((x) => x.id === args.template);
      if (!t) throw new Error(`mock: 没有模板 ${args.template}`);
      if (t.needsPr && !args.prNumber) throw new Error("这个模板要处理一个 PR，请填 PR 号");
      if (!args.prNumber && !args.issueNumber && !args.instructions) {
        throw new Error("请给出要处理的 Issue 号，或写一段需求说明");
      }
      const wf = {
        id: state.nextWorkflow++,
        name: `${t.name} · ${args.repo}`,
        template: t.id,
        repo: args.repo,
        issueNumber: args.issueNumber ?? null,
        issueTitle: null,
        instructions: args.instructions ?? null,
        prNumber: args.prNumber ?? null,
        status: "pending",
        detail: null,
        createdAt: now,
        updatedAt: now,
        steps: t.steps.map((s, i) => ({
          id: 1000 + state.nextWorkflow * 10 + i,
          seq: i + 1,
          name: s.name,
          kind: s.kind,
          dependsOn: s.deps,
          adapter: args.adapters?.[i] || s.adapter || null,
          nodeId: args.nodeId ?? "local",
          workdir: args.workdir ?? null,
          model: (args.models?.[i] || "").trim() || args.model || null,
          autoPr: !!args.autoPr,
          prompt: s.kind === "session" ? `（${s.name} 的提示词）` : null,
          status: "pending",
          taskId: null,
          sessionId: null,
          outcome: null,
          detail: null,
          failure: null,
          startedAt: null,
          finishedAt: null,
        })),
      };
      state.workflows.unshift(wf);
      wfEvent(wf, "workflow.created", "info", "已创建工作流（还没启动）");
      return wf;
    },
    workflow_start: ({ id }) => {
      const wf = mustWf(id);
      wf.status = "running";
      wf.detail = "刚启动";
      wfEvent(wf, "workflow.started", "info", "工作流启动");
      wfTick(id);
      return wf;
    },
    workflow_cancel: ({ id }) => {
      const wf = mustWf(id);
      wf.status = "cancelled";
      wf.detail = "已取消";
      for (const s of wf.steps) {
        if (["pending", "running", "waiting"].includes(s.status)) {
          s.status = "skipped";
          s.detail = "工作流已取消";
          s.finishedAt = now;
        }
      }
      wfEvent(wf, "workflow.cancelled", "warning", "工作流已取消");
      return wf;
    },
    workflow_gate: ({ workflowId, stepId, chosenTaskId }) => {
      const wf = mustWf(workflowId);
      const step = wf.steps.find((s) => s.id === stepId);
      const chosen = state.tasks.find((t) => t.id === chosenTaskId);
      step.status = "done";
      step.detail = `选定「${chosen?.title ?? `任务 #${chosenTaskId}`}」`;
      step.outcome = JSON.stringify({ chosenTask: chosenTaskId, chosenTitle: chosen?.title ?? "" });
      step.finishedAt = now;
      wfEvent(wf, "workflow.step_done", "info", `评审完成：选定「${chosen?.title}」，其余方案保持原样`);
      wfTick(workflowId);
      return wf;
    },
    workflow_retry: ({ id }) => {
      const wf = mustWf(id);
      for (const s of wf.steps) {
        if (["failed", "skipped"].includes(s.status)) {
          Object.assign(s, {
            status: "pending", taskId: null, sessionId: null, outcome: null,
            detail: null, failure: null, startedAt: null, finishedAt: null,
          });
        }
      }
      wf.status = "running";
      wf.detail = "重跑失败步骤";
      wfEvent(wf, "workflow.started", "info", "重跑失败步骤");
      wfTick(id);
      return wf;
    },
    // ---------- 自动化规则 ----------
    automation_rules: () =>
      state.rules.map((r) => ({
        ...r,
        runsToday: state.ruleRuns.filter((x) => x.ruleId === r.id).length,
      })),
    automation_runs: () => state.ruleRuns,
    automation_rule_create: (args) => {
      const name = (args.name ?? "").trim();
      const repo = (args.repo ?? "").trim();
      if (!name) throw new Error("给规则起个名字");
      if (!/^[^/]+\/[^/]+$/.test(repo)) throw new Error("仓库要写成 owner/repo");
      let template = "pr-review";
      if (args.kind === "issue-label") {
        if (!args.label?.trim()) throw new Error("填上要盯的标签（Issue 打上它就自动开工）");
        template = args.template ?? "issue-fix";
      } else if (args.kind === "pr-check-failed") {
        template = "ci-fix";
      } else if (args.kind !== "new-pr") {
        throw new Error("规则类型只有 issue-label（Issue 打标签）、new-pr（新 PR）和 pr-check-failed（检查失败自动修）");
      }
      const rule = {
        id: state.rules.length + 1, name, kind: args.kind, repo, label: args.label?.trim() ?? "",
        template, adapters: args.adapters ?? [], models: args.models ?? [], model: args.model ?? null,
        autoPr: !!args.autoPr, comment: true,
        nodeId: args.nodeId ?? "local", workdir: args.workdir ?? "", enabled: true, runsToday: 0,
      };
      state.rules.unshift(rule);
      return rule;
    },
    automation_rule_set_enabled: ({ id, enabled }) => {
      const rule = state.rules.find((r) => r.id === id);
      if (!rule) throw new Error(`没有这条规则：${id}`);
      rule.enabled = enabled;
      return rule;
    },
    automation_rule_delete: ({ id }) => {
      const before = state.rules.length;
      state.rules = state.rules.filter((r) => r.id !== id);
      if (state.rules.length === before) throw new Error(`没有这条规则：${id}`);
    },
    // 扫一遍：每条生效的规则触发一次（mock 里假装正好有一条新的带标签 Issue / 新 PR）；
    // 同一个键不重复触发；PR 审查跑完了就把结论「贴回」
    automation_scan: () => {
      let triggered = 0;
      // 检查失败规则：任务流水线盯着的 PR 检查红了就让原任务修（按任务+提交去重）
      for (const rule of state.rules.filter((r) => r.enabled && r.kind === "pr-check-failed")) {
        for (const t of state.tasks.filter(
          (x) => x.status === "reviewing" && x.repo === rule.repo && x.prNumber != null
            && (x.checks ?? []).some((c) => c.state === "failure") && x.headSha,
        )) {
          const key = `check:${t.id}:${t.headSha}`;
          if (state.ruleRuns.some((x) => x.ruleId === rule.id && x.key === key)) continue;
          handlers.task_fix({ id: t.id });
          state.ruleRuns.unshift({
            id: state.ruleRuns.length + 1, ruleId: rule.id, ruleName: rule.name,
            key, workflowId: null, posted: false, createdAt: now,
          });
          state.events.unshift({
            id: state.events.length + 1, ts: now, kind: "automation.triggered", level: "info",
            source: rule.name,
            message: `PR #${t.prNumber} 的检查红了，已让原来那个 Agent 修（任务 #${t.id}）`,
            link: `#/tasks?t=${t.id}`,
          });
          triggered += 1;
        }
      }
      for (const rule of state.rules.filter((r) => r.enabled && r.kind !== "pr-check-failed")) {
        const key = rule.kind === "issue-label" ? "issue:9" : "pr:12";
        if (state.ruleRuns.some((x) => x.ruleId === rule.id && x.key === key)) continue;
        const args =
          rule.kind === "issue-label"
            ? { template: rule.template, repo: rule.repo, issueNumber: 9, issueTitle: "首页加载太慢", instructions: null }
            : { template: "pr-review", repo: rule.repo, prNumber: 12, instructions: null };
        const wf = handlers.workflow_create({
          ...args, nodeId: rule.nodeId, workdir: rule.workdir || "/Users/tester/code/demo",
          model: rule.model, autoPr: rule.autoPr, adapters: rule.adapters, models: rule.models ?? [],
        });
        handlers.workflow_start({ id: wf.id });
        state.ruleRuns.unshift({
          id: state.ruleRuns.length + 1, ruleId: rule.id, ruleName: rule.name,
          key, workflowId: wf.id, posted: false, createdAt: now,
        });
        state.events.unshift({
          id: state.events.length + 1, ts: now, kind: "automation.triggered", level: "info",
          source: rule.name,
          message: `规则触发：${key} → 工作流 #${wf.id}`,
          link: `#/workflows?w=${wf.id}`,
        });
        triggered += 1;
      }
      // 新 PR 规则的审查流水线跑完的：结论贴回 PR 评论
      for (const run of state.ruleRuns.filter((x) => !x.posted && x.workflowId != null)) {
        const rule = state.rules.find((r) => r.id === run.ruleId);
        const wf = state.workflows.find((w) => w.id === run.workflowId);
        if (!rule || rule.kind !== "new-pr" || !wf || wf.status !== "done") continue;
        run.posted = true;
        state.events.unshift({
          id: state.events.length + 1, ts: now, kind: "automation.commented", level: "info",
          source: wf.repo, message: `PR #${wf.prNumber} 的自动审查结论已贴成评论`,
          link: `#/workflows?w=${wf.id}`,
        });
        triggered += 1;
      }
      return triggered;
    },
    // ---------- 决策中心 ----------
    inbox_list: () => {
      const gates = [];
      for (const wf of state.workflows) {
        for (const s of wf.steps.filter((x) => x.kind === "gate" && x.status === "waiting")) {
          gates.push({ workflowId: wf.id, workflowName: wf.name, stepId: s.id, stepName: s.name, repo: wf.repo });
        }
      }
      const pick = (t) => ({
        id: t.id, title: t.title, repo: t.repo, branch: t.branch, detail: t.detail, updatedAt: t.updatedAt,
      });
      const reviews = state.tasks
        .filter((t) => t.status === "reviewing" && t.prNumber != null)
        .map((t) => {
          const states = (t.checks ?? []).map((c) => c.state);
          const checks = states.length === 0 ? null
            : states.includes("failure") ? "failure"
            : states.includes("pending") ? "pending" : "success";
          return {
            id: t.id, title: t.title, repo: t.repo, prNumber: t.prNumber, prUrl: t.prUrl,
            checks, detail: t.detail, updatedAt: t.updatedAt,
          };
        });
      return {
        gates,
        changes: state.tasks.filter((t) => t.status === "changes").map(pick),
        starts: state.tasks.filter((t) => t.status === "pending").map(pick),
        reviews,
      };
    },
    inbox_merge_pr: ({ taskId }) => {
      const task = state.tasks.find((t) => t.id === taskId);
      if (!task) throw new Error(`任务不存在：#${taskId}`);
      if (task.prNumber == null) throw new Error("这个任务还没有开 PR");
      task.status = "done";
      task.detail = "PR 已合并，任务完成";
      state.events.unshift({
        id: state.events.length + 1, ts: now, kind: "task.merged", level: "info",
        source: task.title, message: `PR #${task.prNumber} 已合并`, link: `#/tasks?t=${task.id}`,
      });
      // 合并即部署：这个仓库的目标开着 autoDeploy 的话，自动发起一次部署
      const target = state.deploy.targets.find((t) => t.repo === task.repo && t.autoDeploy);
      if (target) {
        state.events.unshift({
          id: state.events.length + 1, ts: now, kind: "deploy.auto", level: "info",
          source: target.name, message: `PR 合并进 main，自动发起了部署`, link: `#/deploy`,
        });
      }
      return task;
    },
    desktop_states: () => state.desktop,
    desktop_launch: ({ path }) => {
      const one = state.desktop.find((d) => d.path === path);
      if (one) one.running = true;
      return null;
    },
    desktop_quit: ({ path }) => {
      const one = state.desktop.find((d) => d.path === path);
      if (one) one.running = false;
      return null;
    },
    desktop_restart: ({ path }) => {
      const one = state.desktop.find((d) => d.path === path);
      if (one) one.running = true;
      return null;
    },
    chat_sessions: () => state.sessions,
    chat_messages: ({ sessionId }) => state.messages.get(sessionId) ?? [],
    chat_live: () => state.live.map((l) => ({ ...l, nodeId: "local" })),
    agent_models: ({ adapter }) => {
      const m = models[adapter];
      if (!m) throw new Error(`mock: 没有为 ${adapter} 准备模型清单`);
      return { ...m, custom: [...m.custom] };
    },
    model_remember: ({ adapter, model }) => {
      state.remembered.push(`${adapter}|${model}`);
      models[adapter].custom.push(model);
    },
    chat_start: ({ adapter, model, workdir, prompt, nodeId }) => {
      const id = state.nextSession++;
      state.sessions.unshift({ id, adapter, title: prompt.slice(0, 24), model, workdir, nodeId: nodeId ?? "local", createdAt: "2026-11-23 10:00:00" });
      state.messages.set(id, []);
      window.__mock.addUserMessage(id, prompt);
      window.__mock.beginTurn(id);
      return id;
    },
    chat_send: ({ sessionId, prompt }) => {
      window.__mock.addUserMessage(sessionId, prompt);
      window.__mock.beginTurn(sessionId);
      return null;
    },
    chat_stop: () => null,
  };
  // 工作流的模拟引擎：测试通过 wfTick 推进，wfFailNext 注入一次失败
  const mustWf = (id) => {
    const wf = state.workflows.find((w) => w.id === id);
    if (!wf) throw new Error(`工作流不存在：#${id}`);
    return wf;
  };
  const wfEvent = (wf, kind, level, message) => {
    state.events.unshift({
      id: state.events.length + 1, ts: now, kind, level,
      source: wf.name, message, link: `#/workflows?w=${wf.id}`,
    });
  };
  const wfTick = (id) => {
    const wf = mustWf(id);
    const fail = state.wfFailNext.find((f) => f.wfId === id);
    // 0) 刚跑起来的步骤在这里挂上任务 / 会话
    for (const s of wf.steps.filter((x) => x.status === "running")) {
      if (s.kind === "task" && s.taskId == null) {
        const task = makeTask({
          title: `${wf.name} · ${s.name}`, repo: wf.repo, issueNumber: wf.issueNumber, issueUrl: null,
          status: "running", nodeId: s.nodeId, adapter: s.adapter, workdir: s.workdir,
          branch: `${wf.issueNumber != null ? `agent/issue-${wf.issueNumber}` : "agent/wf"}-w${s.seq}`,
        });
        state.tasks.unshift(task);
        s.taskId = task.id;
        wfEvent(wf, "workflow.step_started", "info", `「${s.name}」开始`);
      } else if (s.kind === "session" && s.sessionId == null) {
        s.sessionId = 900 + s.id;
        wfEvent(wf, "workflow.step_started", "info", `「${s.name}」开始`);
      }
    }
    // 1) 跑着的出结果（注入了失败的这一步算失败）
    for (const s of wf.steps.filter((x) => x.status === "running")) {
      s.finishedAt = now;
      if (fail && fail.seq === s.seq) {
        s.status = "failed";
        s.failure = "这一轮没有完成：Agent 没有返回任何内容";
        wfEvent(wf, "workflow.step_failed", "warning", `「${s.name}」失败`);
      } else if (s.kind === "task") {
        const task = state.tasks.find((t) => t.id === s.taskId);
        Object.assign(task, { status: "changes", detail: "2 个文件有改动，等你确认" });
        s.status = "done";
        s.outcome = JSON.stringify({ taskId: task.id, branch: task.branch, prUrl: null, taskStatus: "changes" });
        s.detail = task.detail;
        wfEvent(wf, "workflow.step_done", "info", `「${s.name}」完成`);
      } else {
        s.status = "done";
        s.outcome = `${s.name} 的结论：改动合理，建议补测试。`;
        wfEvent(wf, "workflow.step_done", "info", `「${s.name}」完成`);
      }
    }
    if (fail) state.wfFailNext = state.wfFailNext.filter((f) => f !== fail);
    // 2) 硬依赖断掉的级联跳过；依赖满足的接着启动（一有变动就再看一遍）
    let changed = true;
    while (changed) {
      changed = false;
      for (const s of wf.steps.filter((x) => x.status === "pending")) {
        const depState = (d) => wf.steps.find((x) => x.seq === Number(d.replace("?", "")))?.status;
        if (s.dependsOn.some((d) => !d.startsWith("?") && ["failed", "skipped"].includes(depState(d)))) {
          s.status = "skipped";
          s.detail = "上游失败，跳过";
          s.finishedAt = now;
          wfEvent(wf, "workflow.step_skipped", "warning", `「${s.name}」的上游失败，跳过`);
          changed = true;
          continue;
        }
        const ready = s.dependsOn.every(
          (d) => depState(d) === "done" || (d.startsWith("?") && ["failed", "skipped"].includes(depState(d))),
        );
        if (ready) {
          s.status = s.kind === "gate" ? "waiting" : "running";
          s.startedAt = now;
          if (s.kind === "gate") wfEvent(wf, "workflow.gate", "warning", `「${s.name}」等你决策`);
          changed = true;
        }
      }
    }
    // 3) 工作流整体状态
    const terminal = (st) => ["done", "failed", "skipped"].includes(st);
    if (wf.steps.every((s) => terminal(s.status))) {
      wf.status = wf.steps.some((s) => s.status === "done") ? "done" : "failed";
      wf.detail =
        wf.status === "done" && wf.steps.some((s) => s.status !== "done")
          ? "完成（有步骤失败或被跳过，详情在下面）"
          : wf.status === "done"
            ? null
            : "所有步骤都没能完成";
      wfEvent(wf, wf.status === "done" ? "workflow.done" : "workflow.failed",
        wf.status === "done" ? "info" : "warning",
        wf.status === "done" ? "全部步骤完成" : "所有步骤都没能完成");
    } else if (wf.steps.some((s) => s.status === "waiting")) {
      wf.status = "waiting";
      wf.detail = "等你在候选方案里选一个";
    } else {
      const done = wf.steps.filter((s) => s.status === "done").length;
      wf.status = "running";
      wf.detail = `进行中：${done}/${wf.steps.length} 步完成`;
    }
    wf.updatedAt = now;
  };
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb) => {
      const id = nextId++;
      callbacks.set(id, cb);
      return id;
    },
    unregisterCallback: (id) => callbacks.delete(id),
    metadata: { currentWindow: { label: "main" }, currentWebview: { label: "main" } },
    invoke: async (cmd, args) => {
      state.calls.push({ cmd, args: args ?? {} });
      const h = handlers[cmd];
      if (!h) throw new Error(`mock: 未实现的命令 ${cmd}`);
      // 真实的 Tauri 每次都回一份新的数据；这里的状态是原地改的，不拷贝的话
      // 界面看到的新旧数据是同一个对象，会以为什么都没变
      return JSON.parse(JSON.stringify(h(args ?? {}) ?? null));
    },
  };
  window.__mock = {
    state,
    /** Rust 端推送一个事件（真实环境里是 Tauri 的 emit）。返回送达的监听器个数 */
    emit: (payload) => {
      let n = 0;
      for (const [id, l] of listeners) {
        if (l.event === "chat-event") {
          callbacks.get(id)?.({ event: "chat-event", id: n++, payload });
        }
      }
      return n;
    },
    beginTurn: (sessionId) => {
      state.live.push({ sessionId, text: "", activity: null });
    },
    addUserMessage: (sessionId, content) => {
      state.messages.get(sessionId).push({
        id: state.nextMessage++, sessionId, role: "user", content,
        costUsd: null, inputTokens: null, outputTokens: null, createdAt: "",
      });
    },
    /** Rust 端记录实时输出（窗口不可见、事件丢失时界面靠它显示进度） */
    liveSet: (sessionId, text, activity) => {
      const l = state.live.find((x) => x.sessionId === sessionId);
      if (l) { l.text = text; l.activity = activity; }
    },
    /** Rust 端结束一轮：落库并从运行中摘掉 */
    finishTurn: (sessionId, role, content) => {
      state.live = state.live.filter((x) => x.sessionId !== sessionId);
      state.messages.get(sessionId).push({
        id: state.nextMessage++, sessionId, role, content,
        costUsd: role === "assistant" ? 0.03 : null,
        inputTokens: role === "assistant" ? 100 : null,
        outputTokens: role === "assistant" ? 5 : null,
        createdAt: "",
      });
    },
    /** Rust 端推送一条机器状态变化（真实环境里是 emit("node-event") */
    emitNode: (payload) => {
      for (const [id, l] of listeners) {
        if (l.event === "node-event") callbacks.get(id)?.({ event: "node-event", id, payload });
      }
    },
    /** 加一台远程机器；online=false 且给了 error 时是连不上的机器 */
    addMachine: (id, name, { online = true, error = null, home = `/home/${id}`, agents: list = null } = {}) => {
      state.machines = state.machines.filter((m) => m.id !== id);
      state.remoteAgents.set(id, list ?? [agents[0]]);
      state.machines.push({
        id, name, local: false, online,
        info: online || error ? machineInfo(name, home) : null,
        status: { online, error, sinceMs: online ? Date.now() - 60000 : null, lastSeenMs: online ? Date.now() : Date.now() - 3600_000, rttMs: online ? 42 : null },
        agentCount: (list ?? [agents[0]]).length,
      });
      // 中继的在线名单里只有真的在线的机器
      state.relay.online = state.relay.online.filter((n) => n.nodeId !== id);
      if (online) state.relay.online.push({ nodeId: id, name, os: "macos", sinceMs: Date.now() - 60000, paired: true });
      window.__mock.emitNode({ id, name, online, message: error });
    },
    /** 模拟一台机器凭安装命令接入：它出现在中继上并配对成功 */
    pairPending: (name) => {
      const id = `node-${name}`;
      state.relay.pending = state.relay.pending.filter((p) => p.name !== name);
      window.__mock.addMachine(id, name, { online: true });
      state.events.unshift({
        id: state.events.length + 1, ts: "2026-11-24 01:00:00", kind: "node.paired", level: "info",
        source: name, message: "已配对并添加这台机器", link: "#/machines",
      });
    },
    /** 模拟 Agent 做完一轮：工作树里有了改动，任务进入「待确认」 */
    agentDone: (id) => {
      const task = state.tasks.find((t) => t.id === id);
      Object.assign(task, { status: "changes", detail: "2 个文件有改动，等你确认" });
      state.taskDiffs[id] = {
        worktree: task.worktree, branch: task.branch, baseBranch: task.baseBranch, ahead: 0,
        stat: "1 file changed, 3 insertions(+)",
        entries: [
          { path: "src/app/greet.ts", status: "M" },
          { path: "src/app/new.ts", status: "?" },
        ],
      };
      state.taskPatches = {
        "src/app/greet.ts": "@@ -1,3 +1,4 @@\n import { greet } from \"./lib\";\n-old line\n+new line\n export const page = greet();\n",
        "src/app/new.ts": "--- /dev/null\n+++ b/src/app/new.ts\n@@ -0,0 +1,1 @@\n+export const fresh = 1;\n",
      };
    },
    /** 模拟引擎推进一步：跑着的出结果、依赖满足的接着启动 */
    wfTick: (id) => wfTick(id),
    /** 给某一步注入一次失败（下一次 wfTick 生效） */
    wfFail: (wfId, seq) => state.wfFailNext.push({ wfId, seq }),
    lastCall: (cmd) => [...state.calls].reverse().find((c) => c.cmd === cmd) ?? null,
    listenerCount: () => [...listeners.values()].filter((l) => l.event === "chat-event").length,
    callCount: () => state.calls.length,
  };
};

export { MOCK };

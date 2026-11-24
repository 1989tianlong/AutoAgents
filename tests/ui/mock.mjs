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
  const agents = ["claude-cli", "codex-cli", "grok-cli", "pi"].map((adapter, i) => ({
    id: `local/${adapter}`,
    name: ["Claude CLI", "Codex CLI", "Grok CLI", "Pi Agent"][i],
    kind: "cli",
    level: "L3",
    adapter,
    path: `/bin/${adapter}`,
    version: "1.0",
  }));
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
    app_info: () => ({ name: "autoagents", version: "0.1.0", os: "macos", arch: "aarch64", hostname: "test", home: "/Users/tester" }),
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
    github_status: () => null,
    tasks_list: () => [],
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
    lastCall: (cmd) => [...state.calls].reverse().find((c) => c.cmd === cmd) ?? null,
    listenerCount: () => [...listeners.values()].filter((l) => l.event === "chat-event").length,
    callCount: () => state.calls.length,
  };
};

export { MOCK };

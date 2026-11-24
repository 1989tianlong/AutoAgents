import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import {
  agentModels,
  agentsAll,
  chatLive,
  chatMessages,
  chatSend,
  chatSessions,
  chatStart,
  chatStop,
  machinesList,
  modelRemember,
} from "../lib/api";
import { CHAT_ADAPTERS } from "../lib/types";
import type { ChatEvent, ChatMessage } from "../lib/types";
import ModelPicker from "../components/ModelPicker";
import DirPicker from "../components/DirPicker";
import { Badge, StatusDot } from "../components/ui";

/** 有会话在运行时，与 Rust 端对账的间隔 */
const RECONCILE_MS = 2000;

/** 进行中的一轮流式内容，按会话 id 保存：切走再切回不会丢 */
interface Live {
  text: string;
  activity: string | null;
}

const isChatAdapter = (adapter: string) =>
  (CHAT_ADAPTERS as readonly string[]).includes(adapter);

export default function Sessions() {
  const [params, setParams] = useSearchParams();
  const qc = useQueryClient();
  const navigate = useNavigate();
  const sessions = useQuery({ queryKey: ["chatSessions"], queryFn: chatSessions });
  // 所有机器上的 Agent：新会话可以指定在哪台机器上跑
  const agents = useQuery({ queryKey: ["agentsAll"], queryFn: agentsAll });
  const machines = useQuery({
    queryKey: ["machines"],
    queryFn: machinesList,
    refetchInterval: 15000,
  });

  const [selected, setSelected] = useState<number | null>(null);
  const [adapter, setAdapter] = useState(() => params.get("adapter") ?? "claude-cli");
  /** 新会话所在的机器 */
  const [node, setNode] = useState(() => params.get("node") ?? "local");
  const [picking, setPicking] = useState(false);
  /** 选中的模型 ID；空串表示沿用 Agent 默认 */
  const [model, setModel] = useState("");
  const [workdir, setWorkdir] = useState("");
  const [input, setInput] = useState("");
  const [pendingUser, setPendingUser] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // 新会话已发出、还没拿到会话 id
  const [starting, setStarting] = useState(false);
  const [running, setRunning] = useState<number[]>([]);
  const [live, setLive] = useState<Record<number, Live>>({});
  const selectedRef = useRef<number | null>(null);
  const runningRef = useRef<number[]>([]);
  // 本轮收到过推送事件的会话：它们以推送为准，不被状态快照覆盖
  const pushSeenRef = useRef(new Set<number>());
  // 已经结束过的会话：防止「结束事件早于会话 id 返回」把会话卡在运行中
  const finishedRef = useRef(new Set<number>());
  // 已发出、Rust 还没确认开始的那一轮：对账时先不动它
  const pendingStartRef = useRef(new Set<number>());
  const bottomRef = useRef<HTMLDivElement>(null);

  const messages = useQuery({
    queryKey: ["chatMessages", selected],
    queryFn: () => chatMessages(selected!),
    enabled: selected != null,
  });
  const models = useQuery({
    queryKey: ["agentModels", adapter, node],
    queryFn: () => agentModels(adapter, node),
    staleTime: 60_000,
  });

  const updateRunning = useCallback((update: (ids: number[]) => number[]) => {
    runningRef.current = update(runningRef.current);
    setRunning(runningRef.current);
  }, []);

  /** 一轮结束：取回落库的消息，清掉运行状态与流式内容 */
  const settle = useCallback(
    async (id: number) => {
      finishedRef.current.add(id);
      try {
        qc.setQueryData(["chatMessages", id], await chatMessages(id));
      } catch {
        // 取不到消息也不该让会话一直卡在「运行中」
      }
      updateRunning((ids) => ids.filter((x) => x !== id));
      setLive((m) => {
        const next = { ...m };
        delete next[id];
        return next;
      });
      pushSeenRef.current.delete(id);
      if (selectedRef.current === id) setPendingUser(null);
      qc.invalidateQueries({ queryKey: ["chatSessions"] });
    },
    [qc, updateRunning],
  );

  /** 以 Rust 端的状态为准补齐。窗口不可见时推送事件可能没有送达，
   *  有了这一步，漏掉的开始 / 进度 / 结束都能补回来。 */
  const reconcile = useCallback(async () => {
    let snapshot;
    try {
      snapshot = await chatLive();
    } catch {
      return;
    }
    const active = new Set(snapshot.map((s) => s.sessionId));
    // Rust 已经跑完的会话：结束事件没送达，补做收尾
    for (const id of runningRef.current) {
      if (!active.has(id) && !pendingStartRef.current.has(id)) void settle(id);
    }
    // Rust 还在跑、本地却不知道的会话：补登记
    const missing = [...active].filter((id) => !runningRef.current.includes(id));
    if (missing.length) updateRunning((ids) => [...ids, ...missing]);
    // 没有推送可依赖的会话，用快照显示进度
    setLive((m) => {
      let next: Record<number, Live> | null = null;
      for (const s of snapshot) {
        if (pushSeenRef.current.has(s.sessionId)) continue;
        next ??= { ...m };
        next[s.sessionId] = { text: s.text, activity: s.activity };
      }
      return next ?? m;
    });
  }, [settle, updateRunning]);

  // 订阅 Rust 端的流式事件：所有会话的事件都按会话 id 归档，不受当前查看的会话影响
  useEffect(() => {
    const un = listen<ChatEvent>("chat-event", (e) => {
      const p = e.payload;
      const id = p.sessionId;
      pushSeenRef.current.add(id);
      switch (p.type) {
        case "delta":
          setLive((m) => ({
            ...m,
            [id]: { text: (m[id]?.text ?? "") + p.text, activity: m[id]?.activity ?? null },
          }));
          break;
        case "notice":
          setLive((m) => ({ ...m, [id]: { text: m[id]?.text ?? "", activity: p.text } }));
          break;
        case "replace":
          setLive((m) => ({ ...m, [id]: { text: p.text, activity: null } }));
          break;
        case "done":
        case "error":
          void settle(id);
          break;
      }
    }).catch((e: unknown) => {
      setError(`无法订阅 Agent 事件（将改为定时同步）：${String(e)}`);
      return () => {};
    });
    return () => {
      un.then((f) => f());
    };
  }, [settle]);

  // 打开页面、窗口重新可见、以及有会话在运行期间，定时与 Rust 端对账
  useEffect(() => {
    void reconcile();
    const whenVisible = () => {
      if (document.visibilityState === "visible") void reconcile();
    };
    window.addEventListener("focus", whenVisible);
    document.addEventListener("visibilitychange", whenVisible);
    return () => {
      window.removeEventListener("focus", whenVisible);
      document.removeEventListener("visibilitychange", whenVisible);
    };
  }, [reconcile]);

  const hasRunning = running.length > 0;
  useEffect(() => {
    if (!hasRunning) return;
    void reconcile();
    const timer = window.setInterval(() => void reconcile(), RECONCILE_MS);
    return () => window.clearInterval(timer);
  }, [hasRunning, reconcile]);

  const machine = machines.data?.find((m) => m.id === node);
  const machineNames = Object.fromEntries((machines.data ?? []).map((m) => [m.id, m.name]));
  const machineName = (id: string | undefined) => (id && id !== "local" ? (machineNames[id] ?? id) : "本机");

  // 工作目录默认为那台机器的主目录
  const home = machine?.info?.home;
  useEffect(() => {
    if (home && !workdir) setWorkdir(home);
  }, [home, workdir]);

  // 换一台机器：模型清单与默认目录都跟着换
  const pickMachine = (id: string) => {
    setNode(id);
    setModel("");
    setWorkdir(machines.data?.find((m) => m.id === id)?.info?.home ?? "");
  };

  const cliAgents = agents.data?.filter(
    (a) => a.nodeId === node && a.kind === "cli" && isChatAdapter(a.adapter),
  );
  // 当前选中的 Agent 在本机不可用时，换成第一个可用的
  useEffect(() => {
    if (cliAgents && cliAgents.length > 0 && !cliAgents.some((a) => a.adapter === adapter)) {
      setAdapter(cliAgents[0].adapter);
    }
  }, [cliAgents, adapter]);

  const curSession = sessions.data?.find((s) => s.id === selected);
  const isRunning = selected != null && running.includes(selected);
  const busy = starting || isRunning;
  const curLive = selected != null ? live[selected] : undefined;
  // 自己的消息已从数据库取回之后就不再用临时气泡，避免重复
  const showPending = busy && !!pendingUser && (selected == null || messages.data === undefined);

  const liveText = curLive?.text;
  const liveActivity = curLive?.activity;
  const messageCount = messages.data?.length;
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messageCount, liveText, liveActivity, showPending]);

  // 从活动记录等处跳进来：#/sessions?s=<会话号>。用完就把参数去掉，
  // 免得之后用户切到别的会话时又被拉回来
  useEffect(() => {
    const wanted = Number(params.get("s"));
    if (!wanted || !sessions.data?.some((s) => s.id === wanted)) return;
    openSession(wanted);
    setParams(
      (prev) => {
        prev.delete("s");
        return prev;
      },
      { replace: true },
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [params, sessions.data]);

  const openSession = (id: number | null) => {
    selectedRef.current = id;
    setSelected(id);
    setPendingUser(null);
    setError(null);
  };

  const send = async () => {
    const text = input.trim();
    if (!text || busy) return;
    const chosenModel = model.trim();
    setInput("");
    setError(null);
    setPendingUser(text);
    try {
      if (selected == null) {
        const use = isChatAdapter(adapter) ? adapter : "claude-cli";
        setStarting(true);
        const id = await chatStart(use, chosenModel || null, workdir.trim() || null, text, node);
        setStarting(false);
        selectedRef.current = id;
        setSelected(id);
        // 清单里立刻有这个会话：头部才能显示它的机器、Agent 等信息
        qc.invalidateQueries({ queryKey: ["chatSessions"] });
        pushSeenRef.current.delete(id);
        if (!finishedRef.current.has(id)) updateRunning((ids) => [...ids, id]);
        // 清单之外的模型记下来，下次直接出现在下拉里
        const list = models.data;
        if (chosenModel && list && !list.options.some((o) => o.value === chosenModel)) {
          await modelRemember(use, chosenModel);
          qc.invalidateQueries({ queryKey: ["agentModels", use] });
        }
      } else {
        pushSeenRef.current.delete(selected);
        updateRunning((ids) => (ids.includes(selected) ? ids : [...ids, selected]));
        // 自己的这条消息先显示出来：落库的那份要到这一轮结束才取回
        const optimistic = {
          id: -Date.now(),
          sessionId: selected,
          role: "user" as const,
          content: text,
          costUsd: null,
          inputTokens: null,
          outputTokens: null,
          durationMs: null,
          createdAt: "",
        };
        qc.setQueryData<ChatMessage[]>(["chatMessages", selected], (old) => [...(old ?? []), optimistic]);
        pendingStartRef.current.add(selected);
        try {
          await chatSend(selected, text);
        } catch (e) {
          // 没发出去：把先显示的那条撤掉
          qc.setQueryData<ChatMessage[]>(["chatMessages", selected], (old) =>
            (old ?? []).filter((m) => m.id !== optimistic.id),
          );
          throw e;
        } finally {
          pendingStartRef.current.delete(selected);
        }
      }
    } catch (e) {
      setStarting(false);
      if (selected != null) updateRunning((ids) => ids.filter((x) => x !== selected));
      setPendingUser(null);
      setError(String(e));
    }
  };

  const stop = async () => {
    if (selected == null) return;
    try {
      await chatStop(selected);
    } catch (e) {
      setError(String(e));
    }
  };

  const composer = (
    <Composer
      value={input}
      onChange={setInput}
      onSend={send}
      onStop={stop}
      busy={busy}
      canStop={selected != null}
      placeholder="输入消息，Enter 发送（Shift+Enter 换行）"
    />
  );

  const modelHint = models.isPending
    ? "正在读取那台机器上的模型清单…"
    : models.isError
      ? String(models.error)
      : !models.data || models.data.options.length === 0
        ? "没有从那台机器的配置里读到模型清单，可以直接填写模型 ID"
        : `模型清单来自 ${machine?.name ?? "那台机器"} 上的 ${models.data.source}`;

  return (
    <div className="flex h-full">
      {/* 会话列表 */}
      <div className="flex w-60 shrink-0 flex-col border-r border-slate-800/80">
        <div className="p-3">
          <button
            onClick={() => openSession(null)}
            className="w-full rounded-lg bg-sky-600 px-3 py-2 text-sm font-medium text-white transition hover:bg-sky-500"
          >
            ＋ 新会话
          </button>
        </div>
        <div className="flex-1 overflow-y-auto px-3 pb-3">
          {sessions.data?.map((s) => (
            <button
              key={s.id}
              onClick={() => openSession(s.id)}
              className={`mb-1 w-full rounded-lg px-3 py-2 text-left transition ${
                selected === s.id ? "bg-slate-800" : "hover:bg-slate-800/60"
              }`}
            >
              <div className="truncate text-xs text-slate-300">{s.title}</div>
              <div className="mt-0.5 flex items-center justify-between text-[10px] text-slate-500">
                <span className="truncate">{s.model ?? s.adapter}</span>
                {s.nodeId !== "local" && (
                  <span
                    className="ml-1.5 max-w-16 shrink-0 truncate rounded bg-slate-800 px-1 text-slate-400"
                    title={machineName(s.nodeId)}
                  >
                    {machineName(s.nodeId)}
                  </span>
                )}
                <span className={`shrink-0 pl-2 ${running.includes(s.id) ? "text-sky-400" : ""}`}>
                  {running.includes(s.id) ? "运行中" : s.createdAt.slice(5, 16)}
                </span>
              </div>
            </button>
          ))}
        </div>
      </div>

      {/* 聊天区 */}
      <div className="flex min-w-0 flex-1 flex-col">
        {selected == null ? (
          <div className="mx-auto flex w-full max-w-2xl flex-1 flex-col justify-center p-8">
            <h1 className="text-xl font-semibold text-white">与 Agent 对话</h1>
            <p className="mt-1 text-sm text-slate-400">
              在应用内驱动任意一台已接入机器上的 CLI Agent，上下文自动续接，历史保存在本机。
            </p>

            {starting && pendingUser && (
              <div className="mt-6 space-y-3">
                <Bubble role="user" content={pendingUser} />
                <ActivityLine text="启动中…" />
              </div>
            )}

            <div className="mt-6">
              <div className="mb-2 text-xs text-slate-500">机器</div>
              <div className="flex flex-wrap gap-2">
                {machines.data?.map((m) => (
                  <button
                    key={m.id}
                    onClick={() => pickMachine(m.id)}
                    disabled={!m.online}
                    title={m.online ? undefined : (m.status?.error ?? "不在线")}
                    className={`flex items-center gap-2 rounded-lg border px-3 py-1.5 text-sm transition disabled:cursor-not-allowed disabled:opacity-40 ${
                      node === m.id
                        ? "border-sky-600 bg-sky-950/50 text-white"
                        : "border-slate-700 text-slate-300 hover:border-sky-600"
                    }`}
                  >
                    <StatusDot online={m.online} />
                    {m.name}
                  </button>
                ))}
                {machines.isSuccess && machines.data.length === 1 && (
                  <button
                    onClick={() => navigate("/machines?add=1")}
                    className="rounded-lg px-2 py-1.5 text-sm text-sky-400 transition hover:text-sky-300"
                  >
                    ＋ 接入其它机器
                  </button>
                )}
              </div>
            </div>

            <div className="mt-4">
              <div className="mb-2 text-xs text-slate-500">Agent</div>
              <div className="flex flex-wrap gap-2">
                {cliAgents?.map((a) => (
                  <button
                    key={a.id}
                    onClick={() => {
                      setAdapter(a.adapter);
                      setModel("");
                    }}
                    className={`rounded-lg border px-3 py-1.5 text-sm transition ${
                      adapter === a.adapter
                        ? "border-sky-600 bg-sky-950/50 text-white"
                        : "border-slate-700 text-slate-300 hover:border-sky-600"
                    }`}
                  >
                    {a.name}
                  </button>
                ))}
                {agents.data && cliAgents?.length === 0 && (
                  <span className="text-xs text-slate-500">
                    {machine?.online || node === "local"
                      ? `「${machine?.name ?? "本机"}」上没有发现可对话的 CLI Agent，请到 Agents 页重新扫描`
                      : "这台机器不在线"}
                  </span>
                )}
              </div>
            </div>

            <div className="mt-4">
              <div className="mb-2 text-xs text-slate-500">模型</div>
              <ModelPicker
                options={models.data?.options ?? []}
                custom={models.data?.custom ?? []}
                defaultModel={models.data?.default ?? null}
                value={model}
                onChange={setModel}
              />
              <p className="mt-1.5 text-[11px] text-slate-600">{modelHint}</p>
            </div>

            <div className="mt-4">
              <div className="mb-2 text-xs text-slate-500">工作目录</div>
              <div className="flex gap-2">
                <input
                  value={workdir}
                  onChange={(e) => setWorkdir(e.currentTarget.value)}
                  spellCheck={false}
                  placeholder={home ?? "~"}
                  className="min-w-0 flex-1 rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 font-mono text-xs text-slate-200 placeholder:text-slate-600 focus:border-sky-600 focus:outline-none"
                />
                <button
                  onClick={() => setPicking(true)}
                  disabled={!home}
                  title={home ? "在那台机器上浏览目录" : "连上那台机器之后才能浏览"}
                  className="shrink-0 rounded-lg border border-slate-700 px-3 py-2 text-xs text-slate-300 transition hover:border-sky-600 hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
                >
                  浏览…
                </button>
              </div>
              <p className="mt-1.5 text-[11px] text-slate-600">
                Agent 在这个目录下读写文件、执行命令；留空为那台机器的主目录。
              </p>
            </div>

            {error && <ErrorBanner text={error} onClose={() => setError(null)} />}

            <div className="mt-4">{composer}</div>
          </div>
        ) : (
          <>
            <div className="flex items-center gap-2 border-b border-slate-800/80 px-6 py-2 text-xs text-slate-500">
              <span className="shrink-0">{curSession?.adapter ?? adapter}</span>
              {curSession?.model && <Badge tone="violet">{curSession.model}</Badge>}
              <Badge tone={curSession?.nodeId === "local" ? "sky" : "emerald"}>
                {machineName(curSession?.nodeId)}
              </Badge>
              {isRunning && <Badge tone="sky">运行中</Badge>}
              {curSession?.workdir && (
                <span
                  className="truncate font-mono text-[11px] text-slate-600"
                  title={curSession.workdir}
                >
                  {curSession.workdir}
                </span>
              )}
              <span className="ml-auto max-w-[40%] shrink-0 truncate pl-4 text-slate-600">
                {curSession?.title}
              </span>
            </div>

            <div className="flex-1 overflow-y-auto p-6">
              <div className="mx-auto max-w-3xl space-y-4">
                {messages.data?.map((m) => (
                  <Bubble
                    key={m.id}
                    role={m.role}
                    content={m.content}
                    cost={m.costUsd}
                    inputTokens={m.inputTokens}
                    outputTokens={m.outputTokens}
                  />
                ))}
                {showPending && <Bubble role="user" content={pendingUser!} />}
                {curLive?.text && (
                  <Bubble role="assistant" content={curLive.text} streaming={isRunning} />
                )}
                {isRunning && (
                  <ActivityLine
                    text={curLive?.activity ?? (curLive?.text ? null : "等待响应…")}
                  />
                )}
                <div ref={bottomRef} />
              </div>
            </div>

            {error && <ErrorBanner text={error} onClose={() => setError(null)} />}
            {composer}
          </>
        )}
      </div>

      {picking && home && (
        <DirPicker
          node={node}
          machineName={machine?.name ?? "本机"}
          start={workdir.trim() || home}
          onPick={(path) => {
            setWorkdir(path);
            setPicking(false);
          }}
          onClose={() => setPicking(false)}
        />
      )}
    </div>
  );
}

function Bubble({
  role,
  content,
  cost,
  inputTokens,
  outputTokens,
  streaming,
}: {
  role: ChatMessage["role"];
  content: string;
  cost?: number | null;
  inputTokens?: number | null;
  outputTokens?: number | null;
  streaming?: boolean;
}) {
  if (role === "system") {
    return (
      <div className="whitespace-pre-wrap break-words rounded-lg border border-amber-800/60 bg-amber-950/30 px-4 py-2 text-xs leading-5 text-amber-300">
        {content}
      </div>
    );
  }
  const isUser = role === "user";
  // 有费用显示费用（claude / grok / pi），否则显示 token 用量（codex 只给 token）
  const usage =
    cost != null
      ? `≈ $${cost.toFixed(4)}`
      : inputTokens != null || outputTokens != null
        ? `${inputTokens ?? 0} → ${outputTokens ?? 0} tokens`
        : null;
  return (
    <div className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`max-w-[85%] whitespace-pre-wrap break-words rounded-2xl px-4 py-2.5 text-sm leading-6 ${
          isUser ? "bg-sky-600 text-white" : "bg-slate-800 text-slate-200"
        }`}
      >
        {content}
        {streaming && <span className="animate-pulse">▍</span>}
        {!isUser && usage && <div className="mt-1 text-[10px] text-slate-500">{usage}</div>}
      </div>
    </div>
  );
}

/** Agent 执行过程中的一行状态提示 */
function ActivityLine({ text }: { text: string | null }) {
  if (!text) return null;
  return (
    <div className="flex items-center gap-2 pl-1 text-xs text-slate-500">
      <span className="inline-block h-3 w-3 shrink-0 animate-spin rounded-full border-2 border-slate-600 border-t-sky-400" />
      <span className="truncate">{text}</span>
    </div>
  );
}

function ErrorBanner({ text, onClose }: { text: string; onClose: () => void }) {
  return (
    <div className="mt-4 flex items-start justify-between gap-3 rounded-lg border border-rose-800 bg-rose-950/40 px-4 py-2 text-xs leading-5 text-rose-300">
      <span className="break-all">{text}</span>
      <button onClick={onClose} className="shrink-0 text-rose-500 hover:text-rose-300">
        ✕
      </button>
    </div>
  );
}

function Composer({
  value,
  onChange,
  onSend,
  onStop,
  busy,
  canStop,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  onSend: () => void;
  onStop: () => void;
  busy: boolean;
  canStop: boolean;
  placeholder: string;
}) {
  return (
    <div className="border-t border-slate-800/80 pt-4">
      <div className="flex items-end gap-3">
        <textarea
          value={value}
          onChange={(e) => onChange(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              onSend();
            }
          }}
          rows={2}
          placeholder={placeholder}
          className="flex-1 resize-none rounded-xl border border-slate-700 bg-slate-950 px-4 py-3 text-sm text-slate-200 placeholder:text-slate-600 focus:border-sky-600 focus:outline-none"
        />
        {busy && (
          <button
            onClick={onStop}
            disabled={!canStop}
            title={canStop ? "停止本轮，保留已产出的内容" : "正在启动…"}
            className="rounded-xl border border-rose-700 px-4 py-3 text-sm font-medium text-rose-300 transition hover:bg-rose-950/40 disabled:cursor-not-allowed disabled:opacity-50"
          >
            停止
          </button>
        )}
        <button
          onClick={onSend}
          disabled={busy || !value.trim()}
          className="rounded-xl bg-sky-600 px-5 py-3 text-sm font-medium text-white transition hover:bg-sky-500 disabled:cursor-not-allowed disabled:opacity-50"
        >
          {busy ? "回复中…" : "发送"}
        </button>
      </div>
    </div>
  );
}

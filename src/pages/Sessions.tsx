import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useSearchParams } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { chatMessages, chatSend, chatSessions, chatStart, discoverAgents } from "../lib/api";
import type { ChatEvent } from "../lib/types";
import { Badge } from "../components/ui";

/** 已实现流式会话的 adapter */
const SUPPORTED = ["claude-cli"];

/** claude cli 的模型选项（--model，留空跟随其默认配置） */
const MODELS = [
  { value: "", label: "默认" },
  { value: "sonnet", label: "Sonnet" },
  { value: "opus", label: "Opus" },
  { value: "haiku", label: "Haiku" },
];

export default function Sessions() {
  const [params] = useSearchParams();
  const qc = useQueryClient();
  const sessions = useQuery({ queryKey: ["chatSessions"], queryFn: chatSessions });
  const agents = useQuery({ queryKey: ["agents"], queryFn: discoverAgents });
  const [selected, setSelected] = useState<number | null>(null);
  const [adapter, setAdapter] = useState(params.get("adapter") ?? "claude-cli");
  const [model, setModel] = useState("");
  const messages = useQuery({
    queryKey: ["chatMessages", selected],
    queryFn: () => chatMessages(selected!),
    enabled: selected != null,
  });

  const [input, setInput] = useState("");
  const [pendingUser, setPendingUser] = useState<string | null>(null);
  const [streamText, setStreamText] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const selectedRef = useRef<number | null>(null);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    selectedRef.current = selected;
  }, [selected]);

  // 订阅 Rust 端流式事件（只处理当前会话）
  useEffect(() => {
    const un = listen<ChatEvent>("chat-event", (e) => {
      const p = e.payload;
      if (p.sessionId !== selectedRef.current) return;
      if (p.type === "delta") {
        setStreamText((t) => (t ?? "") + p.text);
      } else {
        setStreamText(null);
        setPendingUser(null);
        setBusy(false);
        if (p.type === "error") setError(p.message);
        qc.invalidateQueries({ queryKey: ["chatMessages", p.sessionId] });
        qc.invalidateQueries({ queryKey: ["chatSessions"] });
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, [qc]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.data?.length, streamText, pendingUser]);

  const send = async () => {
    const text = input.trim();
    if (!text || busy) return;
    setInput("");
    setBusy(true);
    setError(null);
    setPendingUser(text);
    try {
      if (selected == null) {
        const use = SUPPORTED.includes(adapter) ? adapter : "claude-cli";
        const id = await chatStart(use, model || null, text);
        selectedRef.current = id; // 立即更新，避免错过早期流事件
        setSelected(id);
      } else {
        await chatSend(selected, text);
      }
    } catch (e) {
      setBusy(false);
      setPendingUser(null);
      setError(String(e));
    }
  };

  const cliAgents = agents.data?.filter((a) => a.kind === "cli" && a.level !== "L1") ?? [];
  const curSession = sessions.data?.find((s) => s.id === selected);

  return (
    <div className="flex h-full">
      {/* 会话列表 */}
      <div className="flex w-60 shrink-0 flex-col border-r border-slate-800/80">
        <div className="p-3">
          <button
            onClick={() => {
              setSelected(null);
              setPendingUser(null);
              setStreamText(null);
              setError(null);
            }}
            className="w-full rounded-lg bg-sky-600 px-3 py-2 text-sm font-medium text-white transition hover:bg-sky-500"
          >
            ＋ 新会话
          </button>
        </div>
        <div className="flex-1 overflow-y-auto px-3 pb-3">
          {sessions.data?.map((s) => (
            <button
              key={s.id}
              onClick={() => {
                selectedRef.current = s.id;
                setSelected(s.id);
                setError(null);
              }}
              className={`mb-1 w-full rounded-lg px-3 py-2 text-left transition ${
                selected === s.id ? "bg-slate-800" : "hover:bg-slate-800/60"
              }`}
            >
              <div className="truncate text-xs text-slate-300">{s.title}</div>
              <div className="mt-0.5 flex items-center justify-between text-[10px] text-slate-500">
                <span>{s.adapter}</span>
                <span>{s.createdAt.slice(5, 16)}</span>
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
              当前支持 claude-cli 流式对话（headless 模式），上下文通过 --resume
              保持，历史保存在本机 SQLite。
            </p>

            <div className="mt-6">
              <div className="mb-2 text-xs text-slate-500">选择 Agent</div>
              <div className="flex flex-wrap gap-2">
                {cliAgents.length === 0 && (
                  <span className="text-xs text-slate-500">未发现可对话的 CLI Agent</span>
                )}
                {cliAgents.map((a) => {
                  const ok = SUPPORTED.includes(a.adapter);
                  return (
                    <button
                      key={a.id}
                      disabled={!ok}
                      onClick={() => setAdapter(a.adapter)}
                      title={ok ? "选择该 Agent" : "适配即将推出"}
                      className={`rounded-lg border px-3 py-1.5 text-sm transition ${
                        adapter === a.adapter
                          ? "border-sky-600 bg-sky-950/50 text-white"
                          : "border-slate-700 text-slate-300"
                      } enabled:hover:border-sky-600 disabled:cursor-not-allowed disabled:opacity-40`}
                    >
                      {a.name}
                    </button>
                  );
                })}
              </div>
            </div>

            <div className="mt-4">
              <div className="mb-2 text-xs text-slate-500">模型</div>
              <select
                value={model}
                onChange={(e) => setModel(e.currentTarget.value)}
                className="w-56 rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 focus:border-sky-600 focus:outline-none"
              >
                {MODELS.map((m) => (
                  <option key={m.value} value={m.value}>
                    {m.label}
                    {m.value ? `（--model ${m.value}）` : "（跟随 Claude 配置）"}
                  </option>
                ))}
              </select>
            </div>

            <Composer
              value={input}
              onChange={setInput}
              onSend={send}
              busy={busy}
              placeholder="输入消息，Enter 发送（Shift+Enter 换行）"
            />
          </div>
        ) : (
          <>
            <div className="flex items-center gap-2 border-b border-slate-800/80 px-6 py-2 text-xs text-slate-500">
              <span>{curSession?.adapter ?? adapter}</span>
              {curSession?.model && <Badge tone="violet">{curSession.model}</Badge>}
              <span className="ml-auto truncate pr-2 text-slate-600">
                {curSession?.title ?? ""}
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
                  />
                ))}
                {pendingUser && <Bubble role="user" content={pendingUser} />}
                {streamText != null && (
                  <Bubble role="assistant" content={streamText} streaming />
                )}
                {error && (
                  <div className="rounded-lg border border-rose-800 bg-rose-950/40 px-4 py-2 text-xs text-rose-300">
                    {error}
                  </div>
                )}
                <div ref={bottomRef} />
              </div>
            </div>
            <Composer
              value={input}
              onChange={setInput}
              onSend={send}
              busy={busy}
              placeholder="输入消息，Enter 发送（Shift+Enter 换行）"
            />
          </>
        )}
      </div>
    </div>
  );
}

function Bubble({
  role,
  content,
  cost,
  streaming,
}: {
  role: "user" | "assistant" | "system";
  content: string;
  cost?: number | null;
  streaming?: boolean;
}) {
  if (role === "system") {
    return (
      <div className="rounded-lg border border-amber-800/60 bg-amber-950/30 px-4 py-2 text-xs text-amber-300">
        {content}
      </div>
    );
  }
  const isUser = role === "user";
  return (
    <div className={`flex ${isUser ? "justify-end" : "justify-start"}`}>
      <div
        className={`max-w-[85%] whitespace-pre-wrap break-words rounded-2xl px-4 py-2.5 text-sm leading-6 ${
          isUser ? "bg-sky-600 text-white" : "bg-slate-800 text-slate-200"
        }`}
      >
        {content}
        {streaming && <span className="animate-pulse">▍</span>}
        {!isUser && cost != null && (
          <div className="mt-1 text-[10px] text-slate-500">≈ ${cost.toFixed(4)}</div>
        )}
      </div>
    </div>
  );
}

function Composer({
  value,
  onChange,
  onSend,
  busy,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  onSend: () => void;
  busy: boolean;
  placeholder: string;
}) {
  return (
    <div className="mt-6 border-t border-slate-800/80 p-4">
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

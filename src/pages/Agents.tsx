import { useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { agentsAll } from "../lib/api";
import { CHAT_ADAPTERS } from "../lib/types";
import type { AgentEntry } from "../lib/types";
import { Badge, Empty, LevelBadge, PageHeader, StatusDot } from "../components/ui";

export default function Agents() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const {
    data: agents,
    isFetching,
    error,
  } = useQuery({ queryKey: ["agentsAll"], queryFn: agentsAll });

  // 按机器分组：本机永远排第一，其余按名字
  const groups = useMemo(() => {
    const byMachine = new Map<string, AgentEntry[]>();
    for (const agent of agents ?? []) {
      const list = byMachine.get(agent.nodeId) ?? [];
      list.push(agent);
      byMachine.set(agent.nodeId, list);
    }
    return [...byMachine.entries()].sort(([a], [b]) => {
      if (a === "local") return -1;
      if (b === "local") return 1;
      const name = (id: string) => byMachine.get(id)?.[0]?.nodeName ?? id;
      return name(a).localeCompare(name(b));
    });
  }, [agents]);

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="Agents"
        desc="所有机器上的 Agent 汇总在这里：本机实时扫描，其它机器连上时同步"
        actions={
          <button
            onClick={() => qc.invalidateQueries({ queryKey: ["agentsAll"] })}
            className="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 transition hover:border-sky-600 hover:text-white"
          >
            {isFetching ? "扫描中…" : "重新扫描"}
          </button>
        }
      />

      {error ? (
        <Empty title="扫描失败" desc={String(error)}>
          <button
            onClick={() => qc.invalidateQueries({ queryKey: ["agentsAll"] })}
            className="rounded-lg bg-sky-600 px-4 py-2 text-sm font-medium text-white hover:bg-sky-500"
          >
            重试
          </button>
        </Empty>
      ) : !agents ? (
        <Empty title="正在扫描…" />
      ) : agents.length === 0 ? (
        <Empty
          title="未发现任何 Agent"
          desc="安装 claude / codex / grok / pi 等 CLI，或把桌面 Agent 装入 /Applications 后重新扫描"
        />
      ) : (
        <div className="space-y-6">
          {groups.map(([nodeId, list]) => {
            // 同一台机器的条目在线状态一致
            const online = list[0].online;
            return (
              <section key={nodeId}>
                <div className="mb-2 flex items-center gap-2 px-1">
                  <StatusDot online={online} />
                  <h2 className="text-sm font-medium text-white">{list[0].nodeName}</h2>
                  <Badge tone={online ? "emerald" : "slate"}>
                    {online ? `${list.length} 个` : "离线 · 上次同步"}
                  </Badge>
                </div>
                <div className="overflow-hidden rounded-xl border border-slate-800">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b border-slate-800 bg-slate-900/80 text-left text-xs text-slate-400">
                        <th className="px-4 py-3 font-medium">Agent</th>
                        <th className="px-4 py-3 font-medium">类型</th>
                        <th className="px-4 py-3 font-medium">能力</th>
                        <th className="px-4 py-3 font-medium">版本</th>
                        <th className="px-4 py-3 font-medium">路径</th>
                        <th className="px-4 py-3 font-medium">操作</th>
                      </tr>
                    </thead>
                    <tbody className="divide-y divide-slate-800/70 bg-slate-900/40">
                      {list.map((a) => {
                        const chatOk = (CHAT_ADAPTERS as readonly string[]).includes(a.adapter);
                        return (
                          <tr key={`${a.nodeId}:${a.id}`} className="transition hover:bg-slate-800/30">
                            <td className="px-4 py-3">
                              <div className="font-medium text-white">{a.name}</div>
                              <div className="text-xs text-slate-500">{a.adapter}</div>
                            </td>
                            <td className="px-4 py-3">
                              <Badge tone={a.kind === "cli" ? "sky" : "violet"}>
                                {a.kind === "cli" ? "CLI" : "桌面"}
                              </Badge>
                            </td>
                            <td className="px-4 py-3">
                              <LevelBadge level={a.level} />
                            </td>
                            <td className="px-4 py-3 text-xs text-slate-400">{a.version ?? "—"}</td>
                            <td
                              className="max-w-64 truncate px-4 py-3 font-mono text-xs text-slate-500"
                              title={a.path}
                            >
                              {a.path}
                            </td>
                            <td className="px-4 py-3">
                              <button
                                onClick={() =>
                                  chatOk && navigate(`/sessions?adapter=${a.adapter}&node=${a.nodeId}`)
                                }
                                disabled={!chatOk || !online}
                                title={
                                  !online
                                    ? "这台机器不在线"
                                    : chatOk
                                      ? `在「${a.nodeName}」上开启托管会话`
                                      : a.kind === "desktop"
                                        ? "桌面 Agent 的深度控制尚未支持"
                                        : "该 Agent 暂不支持会话"
                                }
                                className="rounded-md border border-slate-700 px-2 py-1 text-xs text-slate-300 transition enabled:hover:border-sky-600 enabled:hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
                              >
                                {chatOk ? "💬 会话" : "会话"}
                              </button>
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </section>
            );
          })}
        </div>
      )}
    </div>
  );
}

import { useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { agentsAll, desktopLaunch, desktopQuit, desktopRestart, desktopStates } from "../lib/api";
import { CHAT_ADAPTERS } from "../lib/types";
import type { AgentEntry } from "../lib/types";
import { Badge, Empty, LevelBadge, PageHeader, StatusDot } from "../components/ui";

/** 桌面 Agent 的进程控制（目前只有本机 macOS 支持） */
function DesktopControls({ agent }: { agent: AgentEntry }) {
  const qc = useQueryClient();
  const local = agent.nodeId === "local";
  const states = useQuery({
    queryKey: ["desktopStates"],
    queryFn: desktopStates,
    refetchInterval: 15000,
    enabled: local,
  });
  const running = local && ((states.data ?? []).find((s) => s.path === agent.path)?.running ?? false);
  const refresh = { onSuccess: () => qc.invalidateQueries({ queryKey: ["desktopStates"] }) };
  const launch = useMutation({ mutationFn: () => desktopLaunch(agent.nodeId, agent.path, agent.name), ...refresh });
  const quit = useMutation({ mutationFn: () => desktopQuit(agent.nodeId, agent.path, agent.name), ...refresh });
  const restart = useMutation({ mutationFn: () => desktopRestart(agent.nodeId, agent.path, agent.name), ...refresh });
  const busy = launch.isPending || quit.isPending || restart.isPending;
  const btn =
    "rounded-md border border-slate-700 px-2 py-1 text-xs text-slate-300 transition enabled:hover:border-sky-600 enabled:hover:text-white disabled:cursor-not-allowed disabled:opacity-40";
  const err = [launch.error, quit.error, restart.error].find(Boolean);
  return (
    <div className="flex items-center gap-1.5">
      {!local ? null : (
        <span data-testid="desktop-running" className={running ? "text-xs text-emerald-400" : "text-xs text-slate-500"}>
          {running ? "● 运行中" : "○ 未运行"}
        </span>
      )}
      {running ? (
        <button data-testid="desktop-quit" className={btn} disabled={!local || busy} onClick={() => quit.mutate()}>
          停止
        </button>
      ) : (
        <button data-testid="desktop-launch" className={btn} disabled={!local || busy} onClick={() => launch.mutate()}>
          启动
        </button>
      )}
      <button
        data-testid="desktop-restart"
        className={btn}
        disabled={!local || busy || !running}
        title={local ? undefined : "桌面 Agent 的控制目前只支持本机"}
        onClick={() => restart.mutate()}
      >
        重启
      </button>
      {err ? <span className="text-xs text-rose-400">{String(err)}</span> : null}
    </div>
  );
}

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
      // 固定按中文规则排：不指定的话，中文名和外文名谁在前取决于系统（macOS 与 Linux 的结果就不一样）
      return name(a).localeCompare(name(b), "zh");
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
                              {a.kind === "desktop" ? (
                                <DesktopControls agent={a} />
                              ) : (
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
                                      : "该 Agent 暂不支持会话"
                                }
                                className="rounded-md border border-slate-700 px-2 py-1 text-xs text-slate-300 transition enabled:hover:border-sky-600 enabled:hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
                              >
                                {chatOk ? "💬 会话" : "会话"}
                              </button>
                                )}
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

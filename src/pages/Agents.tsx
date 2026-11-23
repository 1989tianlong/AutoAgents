import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { discoverAgents } from "../lib/api";
import { Badge, Empty, LevelBadge, PageHeader } from "../components/ui";

export default function Agents() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const {
    data: agents,
    isFetching,
    error,
  } = useQuery({ queryKey: ["agents"], queryFn: discoverAgents });

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="Agents"
        desc="本机已发现的 Agent（多机 Agent 将在 M2 汇聚到这里）"
        actions={
          <button
            onClick={() => qc.invalidateQueries({ queryKey: ["agents"] })}
            className="rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 transition hover:border-sky-600 hover:text-white"
          >
            {isFetching ? "扫描中…" : "重新扫描"}
          </button>
        }
      />

      {error ? (
        <Empty title="扫描失败" desc={String(error)}>
          <button
            onClick={() => qc.invalidateQueries({ queryKey: ["agents"] })}
            className="rounded-lg bg-sky-600 px-4 py-2 text-sm font-medium text-white hover:bg-sky-500"
          >
            重试
          </button>
        </Empty>
      ) : !agents ? (
        <Empty title="正在扫描本机…" />
      ) : agents.length === 0 ? (
        <Empty
          title="未发现任何 Agent"
          desc="安装 claude / codex / grok / pi 等 CLI，或把桌面 Agent 装入 /Applications 后重新扫描"
        />
      ) : (
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
              {agents.map((a) => (
                <tr key={a.id} className="transition hover:bg-slate-800/30">
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
                      onClick={() => navigate(`/sessions?adapter=${a.adapter}`)}
                      disabled={a.adapter !== "claude-cli"}
                      title={
                        a.adapter === "claude-cli"
                          ? "开启托管会话（流式对话）"
                          : a.kind === "desktop"
                            ? "桌面 Agent 深度控制将在 M4 支持"
                            : "该 Agent 的会话适配将在后续版本开放"
                      }
                      className="rounded-md border border-slate-700 px-2 py-1 text-xs text-slate-300 transition enabled:hover:border-sky-600 enabled:hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
                    >
                      {a.adapter === "claude-cli" ? "💬 会话" : "会话"}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { usageBudgetSet, usageSummary } from "../lib/api";
import type { UsageBucket } from "../lib/types";
import { Badge, PageHeader } from "../components/ui";

const usd = (v: number) => `$${v.toFixed(2)}`;
const num = (v: number) => (v >= 1e6 ? `${(v / 1e6).toFixed(1)}M` : v >= 1e3 ? `${(v / 1e3).toFixed(1)}k` : String(v));

/** 一列「名字 · 轮次 · 花费 · token」的表 */
function BucketTable({ title, rows, testid }: { title: string; rows?: UsageBucket[]; testid: string }) {
  return (
    <section className="rounded-xl border border-slate-800" data-testid={testid}>
      <h2 className="border-b border-slate-800 px-4 py-3 text-sm font-medium text-white">{title}</h2>
      {rows?.length ? (
        <table className="w-full text-sm">
          <tbody className="divide-y divide-slate-800/70">
            {rows.map((r) => (
              <tr key={r.name} className="bg-slate-900/40">
                <td className="px-4 py-2.5">
                  <span className="font-medium text-slate-200">{r.name}</span>
                  <span className="ml-2 text-xs text-slate-500">{r.turns} 轮</span>
                </td>
                <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-300">{usd(r.costUsd)}</td>
                <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-500">{num(r.inputTokens + r.outputTokens)} tok</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="px-4 py-6 text-center text-xs text-slate-500">还没有记录</p>
      )}
    </section>
  );
}

/** 每日预算：进度条 + 设置 */
function Budget({ spent, budget }: { spent: number; budget: number | null }) {
  const qc = useQueryClient();
  const [input, setInput] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    if (budget != null) setInput(budget.toString());
  }, [budget]);
  const save = useMutation({
    mutationFn: () => usageBudgetSet(Number(input.trim())),
    onSuccess: async () => {
      setNotice("已保存；超了之后自动化不再自动开工，手动的不拦");
      await qc.invalidateQueries({ queryKey: ["usage"] });
    },
    onError: (e) => setNotice(String(e)),
  });
  const pct = budget ? Math.min(100, (spent / budget) * 100) : 0;
  const over = budget != null && spent >= budget;
  return (
    <section className="rounded-xl border border-slate-800 p-4" data-testid="usage-budget">
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-medium text-white">每日预算</h2>
        <Badge tone={over ? "rose" : budget != null ? "emerald" : "slate"}>
          {budget == null ? "未设置" : over ? "已超" : `${pct.toFixed(0)}%`}
        </Badge>
      </div>
      {budget != null ? (
        <>
          <div className="mt-3 h-2 overflow-hidden rounded-full bg-slate-800">
            <div
              className={`h-full rounded-full transition-all ${over ? "bg-rose-500" : "bg-emerald-500"}`}
              style={{ width: `${pct}%` }}
              data-testid="usage-budget-bar"
            />
          </div>
          <p className="mt-2 text-xs text-slate-400">
            今天 {usd(spent)} / 预算 {usd(budget)}
          </p>
        </>
      ) : (
        <p className="mt-2 text-xs text-slate-500">设了预算，超了之后自动化规则和守护修复就不再自动开工（手动发起的不拦），事件里会说清楚</p>
      )}
      <div className="mt-3 flex items-center gap-2">
        <input
          value={input}
          onChange={(e) => setInput(e.currentTarget.value)}
          placeholder="例如 5"
          className="w-28 rounded-lg border border-slate-700 bg-slate-900 px-2.5 py-1.5 text-sm text-slate-200 outline-none focus:border-sky-600"
          data-testid="usage-budget-input"
        />
        <button
          onClick={() => save.mutate()}
          disabled={save.isPending || !input.trim()}
          className="rounded-lg bg-sky-600 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-sky-500 disabled:opacity-40"
          data-testid="usage-budget-save"
        >
          保存
        </button>
        {notice && <span className="text-xs text-slate-400">{notice}</span>}
      </div>
    </section>
  );
}

export default function Usage() {
  const usage = useQuery({ queryKey: ["usage", 14], queryFn: () => usageSummary(14), refetchInterval: 30000 });
  const d = usage.data;
  const days = d?.byDay ?? [];
  const byCost = days.some((x) => x.costUsd > 0);
  const peak = Math.max(1e-9, ...days.map((x) => (byCost ? x.costUsd : x.tokens)));

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="用量" desc="Agent 花了多少、花在哪：按仓库 / 规则 / Agent / 机器汇总，预算超了自动踩刹车" />

      <div className="grid grid-cols-2 gap-4 xl:grid-cols-4">
        {[
          { label: "今天花费", value: d ? usd(d.todayCostUsd) : "…", testid: "usage-today" },
          { label: "近 24 小时", value: d ? `${usd(d.last24hCostUsd)} · ${d.last24hTurns} 轮` : "…" },
          { label: "累计花费", value: d ? usd(d.costUsd) : "…" },
          { label: "累计 token", value: d ? num(d.inputTokens + d.outputTokens) : "…" },
        ].map((c) => (
          <div key={c.label} className="rounded-xl border border-slate-800 p-4" data-testid={c.testid ?? ""}>
            <div className="text-xs text-slate-500">{c.label}</div>
            <div className="mt-1 font-mono text-xl text-white">{c.value}</div>
          </div>
        ))}
      </div>

      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Budget spent={d?.todayCostUsd ?? 0} budget={d?.dailyBudgetUsd ?? null} />
        <section className="rounded-xl border border-slate-800 p-4" data-testid="usage-trend">
          <h2 className="text-sm font-medium text-white">近 {days.length || 14} 天</h2>
          <div className="mt-3 flex h-24 items-end gap-1">
            {days.map((x) => {
              const v = byCost ? x.costUsd : x.tokens;
              return (
                <div key={x.day} className="flex-1" title={`${x.day}：${byCost ? usd(x.costUsd) : `${num(x.tokens)} tok`} · ${x.turns} 轮`}>
                  <div className="w-full rounded-sm bg-sky-600/70" style={{ height: `${Math.max(2, (v / peak) * 88)}px` }} />
                </div>
              );
            })}
          </div>
          <p className="mt-2 text-xs text-slate-500">{byCost ? "按花费（美元）" : "没有费用口径的 Agent，按 token"}</p>
        </section>
      </div>

      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
        <BucketTable title="按仓库" rows={d?.byRepo} testid="usage-by-repo" />
        <BucketTable title="按自动化规则" rows={d?.byRule} testid="usage-by-rule" />
        <BucketTable title="按 Agent" rows={d?.byAgent} testid="usage-by-agent" />
        <BucketTable title="按机器" rows={d?.byMachine} testid="usage-by-machine" />
      </div>

      <section className="mt-4 rounded-xl border border-slate-800" data-testid="usage-sessions">
        <h2 className="border-b border-slate-800 px-4 py-3 text-sm font-medium text-white">花得最多的会话</h2>
        {d?.topSessions.length ? (
          <table className="w-full text-sm">
            <tbody className="divide-y divide-slate-800/70">
              {d.topSessions.map((r) => (
                <tr key={r.name} className="bg-slate-900/40">
                  <td className="px-4 py-2.5">
                    <span className="font-medium text-slate-200">{r.name}</span>
                    <span className="ml-2 text-xs text-slate-500">{r.turns} 轮</span>
                  </td>
                  <td className="px-4 py-2.5 text-right font-mono text-xs text-slate-300">{usd(r.costUsd)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <p className="px-4 py-6 text-center text-xs text-slate-500">还没有记录</p>
        )}
      </section>
    </div>
  );
}

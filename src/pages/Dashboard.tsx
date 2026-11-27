import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "react-router-dom";
import {
  agentsAll,
  chatLive,
  eventsList,
  getAppInfo,
  githubStatus,
  machinesList,
  tasksList,
  usageSummary,
} from "../lib/api";
import { kindLabel, kindTone } from "../lib/events";
import { fmtAgo, fmtBytes, fmtUsd } from "../lib/format";
import type { AppEvent } from "../lib/types";
import { Badge, Card, PageHeader, StatCard, StatusDot } from "../components/ui";

export default function Dashboard() {
  const navigate = useNavigate();
  const { data: info } = useQuery({ queryKey: ["appInfo"], queryFn: getAppInfo });
  const machines = useQuery({
    queryKey: ["machines"],
    queryFn: machinesList,
    refetchInterval: 15000,
  });
  const agents = useQuery({ queryKey: ["agentsAll"], queryFn: agentsAll });
  const tasks = useQuery({ queryKey: ["tasks"], queryFn: tasksList });
  const live = useQuery({ queryKey: ["live"], queryFn: chatLive, refetchInterval: 5000 });
  const usage = useQuery({ queryKey: ["usage", 14], queryFn: () => usageSummary(14), refetchInterval: 30000 });
  const events = useQuery({
    queryKey: ["events", 8, null],
    queryFn: () => eventsList(8, null),
    refetchInterval: 5000,
  });
  const { data: ghUser } = useQuery({
    queryKey: ["ghStatus"],
    queryFn: githubStatus,
    retry: false,
  });

  const list = machines.data ?? [];
  const online = list.filter((m) => m.online).length;
  const activeTasks = tasks.data?.filter((t) => t.status === "running" || t.status === "reviewing").length;
  const days = usage.data?.byDay ?? [];
  // 费用口径的 Agent 一个都没有时，改看 token
  const byCost = days.some((d) => d.costUsd > 0);
  const peak = Math.max(1e-9, ...days.map((d) => (byCost ? d.costUsd : d.tokens)));
  const totalTokens = usage.data ? usage.data.inputTokens + usage.data.outputTokens : 0;

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="仪表盘" desc="多机 Agent 管理与协同总览" />

      <div className="grid grid-cols-2 gap-4 xl:grid-cols-4">
        <StatCard
          label="在线机器"
          value={
            machines.data ? (
              <>
                {online}
                <span className="text-base font-normal text-slate-500"> / {list.length}</span>
              </>
            ) : (
              "…"
            )
          }
          sub={`Agent 共 ${agents.data?.length ?? "…"} 个`}
        />
        <StatCard
          label="进行中的对话"
          value={live.data ? live.data.length : "…"}
          sub={
            usage.data
              ? usage.data.last24hTurns > 0
                ? `近 24 小时 ${usage.data.last24hTurns} 轮`
                : "近 24 小时没有对话"
              : ""
          }
        />
        <StatCard
          label="近 24 小时花费"
          value={usage.data ? fmtUsd(usage.data.last24hCostUsd) : "…"}
          sub={
            usage.data
              ? usage.data.costUsd > 0
                ? `累计 ${fmtUsd(usage.data.costUsd)} · ${usage.data.turns} 轮`
                : `${usage.data.turns} 轮 · ${totalTokens.toLocaleString()} tokens`
              : ""
          }
        />
        <StatCard
          label="GitHub"
          value={ghUser ? ghUser.login : "未连接"}
          sub={ghUser ? "已连接" : "去 GitHub 页连接"}
        />
      </div>

      <div className="mt-6 grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card className="p-5">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-medium text-slate-300">机器</h2>
            <Link to="/machines" className="text-xs text-sky-400 hover:text-sky-300">
              查看全部 →
            </Link>
          </div>
          <ul className="mt-3 space-y-2">
            {list.map((m) => (
              <li key={m.id} className="flex items-center gap-3 text-sm">
                <StatusDot online={m.online} />
                <span className="truncate text-white">{m.name}</span>
                <span className="truncate text-xs text-slate-500">
                  {m.info ? `${m.info.os} · ${m.info.arch} · ${fmtBytes(m.info.memTotal)}` : "还没有连上过"}
                </span>
                <span className="ml-auto shrink-0 text-xs text-slate-500">{m.agentCount} 个 Agent</span>
                {m.online ? (
                  <Badge tone="emerald">在线</Badge>
                ) : (
                  <Badge tone="slate">{m.status?.lastSeenMs ? `最后在线 ${fmtAgo(m.status.lastSeenMs)}` : "离线"}</Badge>
                )}
              </li>
            ))}
            {machines.isSuccess && list.length === 1 && (
              <li className="pt-1 text-xs text-slate-500">
                还有工作电脑、家里的电脑或服务器要管？
                <button onClick={() => navigate("/machines?add=1")} className="ml-1 text-sky-400 hover:text-sky-300">
                  接入进来 →
                </button>
              </li>
            )}
          </ul>
        </Card>

        <Card className="p-5">
          <h2 className="text-sm font-medium text-slate-300">快捷操作</h2>
          <div className="mt-4 grid grid-cols-2 gap-3">
            <QuickLink to="/sessions" title="与 Agent 对话" sub="在任意一台机器上发起会话" />
            <QuickLink to="/machines?add=1" title="接入一台机器" sub="一条安装命令，自动配对" />
            <QuickLink to="/github" title="从 Issue 创建任务" sub={ghUser ? `已连接 ${ghUser.login}` : "先连接 GitHub"} />
            <QuickLink to="/deploy" title="配置部署目标" sub="Vercel / Cloudflare" />
          </div>
          {tasks.data && tasks.data.length > 0 && (
            <div className="mt-4 border-t border-slate-800/70 pt-3 text-xs text-slate-500">
              任务 {tasks.data.length} 条
              {activeTasks ? `，进行中 ${activeTasks} 条` : ""}
              <Link to="/tasks" className="ml-2 text-sky-400 hover:text-sky-300">
                查看任务 →
              </Link>
            </div>
          )}
        </Card>
      </div>

      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card className="p-5">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-medium text-slate-300">近 14 天用量</h2>
            <Link to="/usage" className="text-xs text-sky-400 hover:text-sky-300">
              用量详情 →
            </Link>
          </div>
          {usage.data && usage.data.turns === 0 ? (
            <p className="mt-4 text-xs text-slate-500">还没有对话。发起第一轮之后，这里按天显示花费或 token。</p>
          ) : (
            <>
              <div className="mt-4 flex h-28 items-end gap-1.5">
                {days.map((d) => {
                  const value = byCost ? d.costUsd : d.tokens;
                  const height = Math.max(2, (value / peak) * 100);
                  return (
                    <div
                      key={d.day}
                      title={`${d.day}：${d.turns} 轮，${byCost ? fmtUsd(d.costUsd) : `${d.tokens.toLocaleString()} tokens`}`}
                      className="flex-1 rounded-t bg-sky-500/70 transition-all hover:bg-sky-400"
                      style={{ height: `${height}%` }}
                    />
                  );
                })}
              </div>
              <div className="mt-1.5 flex justify-between font-mono text-[10px] text-slate-600">
                <span>{days[0]?.day.slice(5)}</span>
                <span>{days[days.length - 1]?.day.slice(5)}</span>
              </div>
              <div className="mt-3 flex flex-wrap gap-x-5 gap-y-1 text-xs text-slate-500">
                <span>
                  {usage.data?.turns} 轮
                  {usage.data && usage.data.costUsd > 0 ? ` · ${fmtUsd(usage.data.costUsd)}` : ""}
                </span>
                <span>
                  {usage.data?.inputTokens.toLocaleString()} → {usage.data?.outputTokens.toLocaleString()} tokens
                </span>
                {usage.data && usage.data.avgDurationMs > 0 && (
                  <span>平均每轮 {(usage.data.avgDurationMs / 1000).toFixed(1)} 秒</span>
                )}
              </div>
            </>
          )}
        </Card>

        <Card className="p-5">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-medium text-slate-300">最近活动</h2>
            <Link to="/activity" className="text-xs text-sky-400 hover:text-sky-300">
              全部 →
            </Link>
          </div>
          <ul className="mt-3 space-y-2.5 text-sm">
            {events.data?.slice(0, 6).map((event) => (
              <ActivityItem
                key={event.id}
                event={event}
                onOpen={(to) => navigate(to)}
              />
            ))}
            {events.data && events.data.length === 0 && (
              <li className="text-xs text-slate-500">接入机器、发起会话之后，这里会出现最新动态</li>
            )}
          </ul>
          <p className="mt-3 text-[11px] text-slate-600">
            {info ? `AutoAgents v${info.version}` : ""} · 本机 {info?.hostname}
          </p>
        </Card>
      </div>
    </div>
  );
}

function QuickLink({ to, title, sub }: { to: string; title: string; sub: string }) {
  return (
    <Link
      to={to}
      className="rounded-lg border border-slate-800 bg-slate-900 p-3 text-sm text-slate-300 transition hover:border-sky-600 hover:text-white"
    >
      {title}
      <span className="mt-0.5 block text-xs text-slate-500">{sub}</span>
    </Link>
  );
}

function ActivityItem({ event, onOpen }: { event: AppEvent; onOpen: (to: string) => void }) {
  const body = (
    <>
      <span className="w-16 shrink-0 text-right text-xs text-slate-500">{fmtAgo(event.ts)}</span>
      <Badge tone={kindTone(event.kind, event.level)}>{kindLabel(event.kind)}</Badge>
      <span className="min-w-0 flex-1 truncate text-slate-300" title={event.message}>
        {event.message}
      </span>
    </>
  );
  return (
    <li>
      {event.link ? (
        <button onClick={() => onOpen(event.link!.replace(/^#/, ""))} className="flex w-full items-center gap-3 text-left hover:bg-slate-800/40">
          {body}
        </button>
      ) : (
        <div className="flex items-center gap-3">{body}</div>
      )}
    </li>
  );
}

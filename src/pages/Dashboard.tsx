import { useQuery } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { discoverAgents, getAppInfo, githubStatus, tasksList } from "../lib/api";
import { Badge, Card, PageHeader, StatCard } from "../components/ui";

export default function Dashboard() {
  const { data: agents } = useQuery({ queryKey: ["agents"], queryFn: discoverAgents });
  const { data: info } = useQuery({ queryKey: ["appInfo"], queryFn: getAppInfo });
  const { data: tasks } = useQuery({ queryKey: ["tasks"], queryFn: tasksList });
  const { data: ghUser } = useQuery({
    queryKey: ["ghStatus"],
    queryFn: githubStatus,
    retry: false,
  });
  const cli = agents?.filter((a) => a.kind === "cli").length ?? 0;
  const desktop = agents?.filter((a) => a.kind === "desktop").length ?? 0;
  const running = tasks?.filter((t) => t.status === "running" || t.status === "reviewing").length ?? 0;

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="仪表盘" desc="多机 Agent 管理与协同总览" />

      <div className="grid grid-cols-2 gap-4 xl:grid-cols-4">
        <StatCard label="在线机器" value={1} sub="本机" />
        <StatCard
          label="已发现 Agent"
          value={agents ? agents.length : "…"}
          sub={`CLI ${cli} · 桌面 ${desktop}`}
        />
        <StatCard label="进行中任务" value={tasks ? running : "…"} sub={`共 ${tasks?.length ?? 0} 条`} />
        <StatCard
          label="GitHub"
          value={ghUser ? ghUser.login : "未连接"}
          sub={ghUser ? "已连接" : "去 GitHub 页连接"}
        />
      </div>

      <div className="mt-6 grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card className="p-5">
          <div className="flex items-center justify-between">
            <h2 className="text-sm font-medium text-slate-300">机器健康</h2>
            <Link to="/machines" className="text-xs text-sky-400 hover:text-sky-300">
              查看全部 →
            </Link>
          </div>
          <div className="mt-4 flex items-center gap-3">
            <span className="relative flex h-2.5 w-2.5">
              <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-500 opacity-60" />
              <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-emerald-500" />
            </span>
            <div className="min-w-0">
              <div className="truncate text-sm text-white">{info?.hostname ?? "本机"}</div>
              <div className="text-xs text-slate-500">
                {info ? `${info.os} · ${info.arch}` : "…"} · agentd 内置
              </div>
            </div>
            <Badge tone="emerald">在线</Badge>
          </div>
        </Card>

        <Card className="p-5">
          <h2 className="text-sm font-medium text-slate-300">快捷操作</h2>
          <div className="mt-4 grid grid-cols-2 gap-3">
            <QuickLink to="/github" title="从 Issue 创建任务" sub="GitHub 接入后开放" />
            <QuickLink to="/agents" title="扫描本机 Agent" sub="发现已安装的 Agent" />
            <QuickLink to="/workflows" title="查看工作流模板" sub="多 Agent 流水线" />
            <QuickLink to="/deploy" title="配置部署目标" sub="Vercel / Cloudflare" />
          </div>
        </Card>
      </div>

      <Card className="mt-4 p-5">
        <h2 className="text-sm font-medium text-slate-300">最近活动</h2>
        <ul className="mt-3 space-y-2.5 text-sm">
          <ActivityItem time="刚刚" text="应用启动，完成本机 Agent 扫描" />
          <ActivityItem time="—" text="连接 GitHub 与多机节点后，这里将展示任务与部署动态" />
        </ul>
      </Card>
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

function ActivityItem({ time, text }: { time: string; text: string }) {
  return (
    <li className="flex items-center gap-3">
      <span className="w-14 shrink-0 text-right text-xs text-slate-500">{time}</span>
      <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-slate-600" />
      <span className="text-slate-300">{text}</span>
    </li>
  );
}

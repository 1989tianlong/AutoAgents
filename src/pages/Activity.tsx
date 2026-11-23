import { Badge, Card, PageHeader } from "../components/ui";

export default function Activity() {
  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="活动" desc="跨机器、Agent、任务的统一事件流（当前仅本机事件）" />

      <Card className="divide-y divide-slate-800/70">
        <Event time="11:20" source="系统" text="AutoAgents v0.1.0 启动（M1 单机骨架）" />
        <Event time="11:20" source="发现" text="完成本机 Agent 扫描：PATH 与 /Applications" />
        <Event
          time="—"
          source="系统"
          text="连接 GitHub / 多机节点后，任务与部署事件将出现在这里"
        />
      </Card>
    </div>
  );
}

function Event({ time, source, text }: { time: string; source: string; text: string }) {
  return (
    <div className="flex items-center gap-3 px-4 py-3 text-sm">
      <span className="w-12 shrink-0 text-right font-mono text-xs text-slate-500">{time}</span>
      <Badge tone="slate">{source}</Badge>
      <span className="text-slate-300">{text}</span>
    </div>
  );
}

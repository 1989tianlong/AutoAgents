import { useQuery } from "@tanstack/react-query";
import { getAppInfo } from "../lib/api";
import { Badge, Card, PageHeader } from "../components/ui";

export default function Machines() {
  const { data: info } = useQuery({ queryKey: ["appInfo"], queryFn: getAppInfo });

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="机器" desc="纳管工作电脑、家庭电脑与服务器（多机接入将在 M2 开放）" />

      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
        <Card className="p-5">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <span className="relative flex h-2.5 w-2.5">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-500 opacity-60" />
                <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-emerald-500" />
              </span>
              <div>
                <div className="text-sm font-medium text-white">{info?.hostname ?? "本机"}</div>
                <div className="text-xs text-slate-500">当前设备</div>
              </div>
            </div>
            <Badge tone="emerald">在线</Badge>
          </div>

          <dl className="mt-4 space-y-2 text-sm">
            <Row label="系统" value={info ? `${info.os} · ${info.arch}` : "…"} />
            <Row label="agentd" value="内置 v0.1（随应用启动）" />
            <Row label="分组" value="本机" />
            <Row label="心跳" value="—" />
          </dl>
        </Card>

        <Card className="grid place-items-center border-dashed p-6 text-center">
          <div>
            <div className="text-sm font-medium text-slate-300">接入新机器</div>
            <p className="mt-1 text-xs leading-5 text-slate-500">
              在目标机器安装 agentd 并输入配对码（M2 开放）
            </p>
            <code className="mt-3 block rounded-md bg-slate-950 px-3 py-1.5 text-xs text-emerald-400">
              curl -fsSL https://autoagents.dev/install.sh | sh
            </code>
          </div>
        </Card>
      </div>
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between gap-4">
      <dt className="text-xs text-slate-500">{label}</dt>
      <dd className="truncate text-slate-300">{value}</dd>
    </div>
  );
}

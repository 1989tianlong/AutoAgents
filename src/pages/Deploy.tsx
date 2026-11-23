import { Fragment } from "react";
import { Badge, Card, PageHeader } from "../components/ui";

const PROVIDERS = [
  {
    name: "Vercel",
    scope: "Preview / Production",
    desc: "Git 集成自动部署，或经 Deployments API 主动触发；PR 自动生成预览链接。",
    tone: "sky" as const,
  },
  {
    name: "Cloudflare",
    scope: "Pages / Workers",
    desc: "Git 集成或 API v4 触发；wrangler 可作为 CLI Agent 由任意机器执行 deploy。",
    tone: "amber" as const,
  },
];

const GUARDIAN_STEPS = [
  "merge 触发部署",
  "健康检查",
  "失败抓取构建日志",
  "自动建修复任务",
  "Agent 修复重发",
];

export default function Deploy() {
  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="部署" desc="Vercel / Cloudflare 自动化发布与守护（M3 上线）" />

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {PROVIDERS.map((p) => (
          <Card key={p.name} className="p-5">
            <div className="flex items-center justify-between">
              <h2 className="font-medium text-white">{p.name}</h2>
              <Badge tone={p.tone}>未连接</Badge>
            </div>
            <div className="mt-1 text-xs text-slate-500">{p.scope}</div>
            <p className="mt-3 text-sm leading-6 text-slate-400">{p.desc}</p>
            <button
              disabled
              className="mt-4 rounded-lg border border-slate-700 px-3 py-1.5 text-sm text-slate-300 disabled:cursor-not-allowed disabled:opacity-40"
            >
              配置 Token
            </button>
          </Card>
        ))}
      </div>

      <Card className="mt-4 p-5">
        <h2 className="text-sm font-medium text-slate-300">守护闭环</h2>
        <div className="mt-4 flex flex-wrap items-center gap-2">
          {GUARDIAN_STEPS.map((s, i) => (
            <Fragment key={s}>
              {i > 0 && <span className="text-slate-600">→</span>}
              <span className="rounded-md border border-slate-700 bg-slate-900 px-2.5 py-1 text-xs text-slate-300">
                {s}
              </span>
            </Fragment>
          ))}
        </div>
        <p className="mt-4 text-xs leading-5 text-slate-500">
          Production 部署默认人工卡点；Preview 全自动。支持一键回滚到任意历史部署。
        </p>
      </Card>
    </div>
  );
}

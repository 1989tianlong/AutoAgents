import { Badge, PageHeader } from "../components/ui";

const COLUMNS = [
  { key: "pending", label: "待处理", tone: "slate" as const },
  { key: "running", label: "进行中", tone: "sky" as const },
  { key: "reviewing", label: "待审查", tone: "amber" as const },
  { key: "done", label: "完成", tone: "emerald" as const },
  { key: "failed", label: "失败", tone: "rose" as const },
];

export default function Tasks() {
  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="任务"
        desc="Issue 转任务、多 Agent 流水线的执行单元（任务引擎随 GitHub 接入开放）"
        actions={
          <button
            disabled
            title="任务引擎将在 GitHub 接入后开放"
            className="rounded-lg bg-sky-600 px-3 py-1.5 text-sm font-medium text-white disabled:cursor-not-allowed disabled:opacity-50"
          >
            ＋ 新建任务
          </button>
        }
      />

      <div className="grid grid-cols-2 gap-4 md:grid-cols-3 xl:grid-cols-5">
        {COLUMNS.map((c) => (
          <div key={c.key}>
            <div className="mb-2 flex items-center justify-between px-1">
              <Badge tone={c.tone}>{c.label}</Badge>
              <span className="text-xs text-slate-500">{c.key === "pending" ? 1 : 0}</span>
            </div>
            <div className="min-h-40 space-y-2 rounded-xl border border-slate-800/70 bg-slate-900/30 p-2">
              {c.key === "pending" && <SampleTask />}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function SampleTask() {
  return (
    <div className="rounded-lg border border-dashed border-slate-700 bg-slate-900/60 p-3">
      <div className="flex items-center gap-2">
        <Badge tone="slate">示例</Badge>
        <span className="text-xs text-slate-400">#12 修复登录超时</span>
      </div>
      <p className="mt-2 text-xs leading-5 text-slate-500">
        GitHub 接入后，Issue 可一键转为任务并分配给 Agent / 工作流执行。
      </p>
    </div>
  );
}

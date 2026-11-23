import { useState } from "react";
import { Card, PageHeader } from "../components/ui";

export default function GitHub() {
  const [pat, setPat] = useState("");

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="GitHub" desc="Issue / PR 看板与 Agent 协作的入口" />

      <Card className="p-5">
        <h2 className="text-sm font-medium text-slate-300">连接 GitHub</h2>
        <p className="mt-1 text-xs leading-5 text-slate-500">
          使用 Personal Access Token（推荐 fine-grained，仅授予目标仓库的 issues / pull
          requests / contents 权限）。Token 仅存本机 Keychain，不离开设备。
        </p>
        <div className="mt-4 flex gap-3">
          <input
            type="password"
            value={pat}
            onChange={(e) => setPat(e.currentTarget.value)}
            placeholder="github_pat_…"
            disabled
            className="flex-1 rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 placeholder:text-slate-600 focus:border-sky-600 focus:outline-none disabled:opacity-50"
          />
          <button
            disabled
            title="将在 M1 后续版本开放"
            className="rounded-lg bg-sky-600 px-4 py-2 text-sm font-medium text-white disabled:cursor-not-allowed disabled:opacity-50"
          >
            连接
          </button>
        </div>
      </Card>

      <div className="mt-4 grid grid-cols-1 gap-4 md:grid-cols-3">
        <FeatureCard
          title="Issue → 任务"
          desc="选中 Issue 一键转任务，自动创建 agent/issue-N 分支并分配 Agent。"
        />
        <FeatureCard
          title="PR 生命周期"
          desc="Agent 产出的 PR 自动打标，审查结论以 review comment 呈现。"
        />
        <FeatureCard
          title="Webhook 自动化"
          desc="Issue 打标签 agent-todo 即自动触发工作流（M3）。"
        />
      </div>
    </div>
  );
}

function FeatureCard({ title, desc }: { title: string; desc: string }) {
  return (
    <Card className="p-4">
      <div className="text-sm font-medium text-slate-200">{title}</div>
      <p className="mt-1.5 text-xs leading-5 text-slate-500">{desc}</p>
    </Card>
  );
}

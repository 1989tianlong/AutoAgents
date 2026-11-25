import { Fragment } from "react";
import { Link } from "react-router-dom";
import { Badge, Card, PageHeader } from "../components/ui";

/** live：这条流水线已经不用编排、直接做在某个页面里了，后面跟的是去哪 */
const TEMPLATES: { name: string; desc: string; steps: string[]; live?: string }[] = [
  {
    name: "修 Issue 流水线",
    desc: "分析修复 → 交叉审查 → 撰写变更说明",
    steps: ["claude-cli", "codex-cli", "grok-cli"],
  },
  {
    name: "PR 审查流水线",
    desc: "新 PR 触发双 Agent 交叉审查并汇总结论",
    steps: ["codex-cli", "claude-cli", "汇总结论"],
  },
  {
    name: "发布守护流水线",
    desc: "生产部署 → 健康检查 → 失败自动建修复任务",
    steps: ["Vercel / CF 部署", "健康检查", "回灌修复"],
    live: "/deploy",
  },
  {
    name: "竞争模式",
    desc: "多 Agent 并行解决同一 Issue，人工择优",
    steps: ["claude-cli", "codex-cli", "人工评审"],
  },
];

export default function Workflows() {
  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="工作流" desc="多 Agent 编排还在做；「发布守护」已经先做进了部署页，不用编排就能跑" />

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {TEMPLATES.map((t) => (
          <Card key={t.name} className="p-5">
            <div className="flex items-center justify-between gap-3">
              <h2 className="font-medium text-white">{t.name}</h2>
              {t.live ? (
                <Link to={t.live} className="text-xs text-emerald-400 hover:text-emerald-300">
                  已上线 · 去部署页 →
                </Link>
              ) : (
                <Badge tone="violet">规划中</Badge>
              )}
            </div>
            <p className="mt-1.5 text-sm text-slate-400">{t.desc}</p>
            <div className="mt-4 flex flex-wrap items-center gap-1.5">
              {t.steps.map((s, i) => (
                <Fragment key={s}>
                  {i > 0 && <span className="text-slate-600">→</span>}
                  <span className="rounded-md border border-slate-700 bg-slate-900 px-2 py-1 text-xs text-slate-300">
                    {s}
                  </span>
                </Fragment>
              ))}
            </div>
          </Card>
        ))}
      </div>
    </div>
  );
}

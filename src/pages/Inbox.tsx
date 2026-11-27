import { useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Card, Empty, PageHeader } from "../components/ui";
import { inboxList, inboxMergePr } from "../lib/api";
import { openSafeUrl } from "../lib/url";

const CHECKS: Record<string, { label: string; tone: "emerald" | "amber" | "rose" | "slate" }> = {
  success: { label: "检查全绿", tone: "emerald" },
  pending: { label: "检查进行中", tone: "amber" },
  failure: { label: "检查未过", tone: "rose" },
};

/** 决策中心：所有等人拍板的事都在一个页面上。这里空着 = 全自动跑着，没有要你操心的事 */
export default function Inbox() {
  const qc = useQueryClient();
  const [busy, setBusy] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const inbox = useQuery({ queryKey: ["inbox"], queryFn: inboxList, refetchInterval: 5000 });
  const data = inbox.data;
  const total = data ? data.gates.length + data.changes.length + data.starts.length + data.reviews.length : 0;

  const merge = async (taskId: number) => {
    setBusy(taskId);
    setError(null);
    try {
      await inboxMergePr(taskId);
      qc.invalidateQueries({ queryKey: ["inbox"] });
      qc.invalidateQueries({ queryKey: ["tasks"] });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="mx-auto max-w-5xl p-8">
      <PageHeader
        title="决策中心"
        desc="所有等你拍板的事聚在这一页：评审门择优、改动确认、任务启动、PR 验收。别的都是自动的"
      />
      {error && <p className="mb-3 text-xs text-rose-400" data-testid="inbox-error">{error}</p>}

      {data == null ? null : total === 0 ? (
        <Empty title="现在没有等你决策的事" desc="自动化规则、任务流水线和工作流都在自己跑；有需要你拍板的会出现在这里。" />
      ) : (
        <div className="space-y-6" data-testid="inbox">
          {/* 人工评审门 */}
          {data.gates.length > 0 && (
            <section>
              <h2 className="mb-2 text-sm font-medium text-slate-300">人工评审（竞争模式择优）</h2>
              <div className="space-y-2">
                {data.gates.map((g) => (
                  <Card key={`${g.workflowId}-${g.stepId}`} className="flex items-center justify-between gap-3 p-4" data-testid="inbox-gate">
                    <div className="min-w-0">
                      <p className="truncate text-sm text-white">{g.workflowName}</p>
                      <p className="mt-0.5 text-xs text-slate-500">{g.repo}</p>
                    </div>
                    <Link to={`/workflows?w=${g.workflowId}`}>
                      <Button variant="primary">去择优 →</Button>
                    </Link>
                  </Card>
                ))}
              </div>
            </section>
          )}

          {/* 改动待确认 */}
          {data.changes.length > 0 && (
            <section>
              <h2 className="mb-2 text-sm font-medium text-slate-300">改动待确认（确认后才提交 / 开 PR）</h2>
              <div className="space-y-2">
                {data.changes.map((t) => (
                  <Card key={t.id} className="flex items-center justify-between gap-3 p-4" data-testid="inbox-change">
                    <div className="min-w-0">
                      <p className="truncate text-sm text-white">#{t.id} {t.title}</p>
                      <p className="mt-0.5 truncate text-xs text-slate-500">
                        {t.repo}
                        {t.branch && ` · ${t.branch}`}
                        {t.detail && ` · ${t.detail}`}
                      </p>
                    </div>
                    <Link to={`/tasks?t=${t.id}`}>
                      <Button variant="primary">看 diff →</Button>
                    </Link>
                  </Card>
                ))}
              </div>
            </section>
          )}

          {/* PR 等验收 */}
          {data.reviews.length > 0 && (
            <section>
              <h2 className="mb-2 text-sm font-medium text-slate-300">PR 等你验收</h2>
              <div className="space-y-2">
                {data.reviews.map((r) => (
                  <Card key={r.id} className="flex items-center justify-between gap-3 p-4" data-testid="inbox-review">
                    <div className="min-w-0">
                      <p className="truncate text-sm text-white">
                        #{r.id} {r.title}
                        <span className="ml-2 text-xs text-slate-500">PR #{r.prNumber}</span>
                      </p>
                      <p className="mt-0.5 flex items-center gap-2 truncate text-xs text-slate-500">
                        {r.repo}
                        {r.detail && ` · ${r.detail}`}
                      </p>
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      {r.checks && <Badge tone={CHECKS[r.checks]?.tone ?? "slate"}>{CHECKS[r.checks]?.label ?? r.checks}</Badge>}
                      {r.prUrl && (
                        <button
                          type="button"
                          onClick={() => openSafeUrl(r.prUrl)}
                          className="text-xs text-sky-400 hover:text-sky-300"
                        >
                          GitHub →
                        </button>
                      )}
                      <Button variant="primary" disabled={busy === r.id} onClick={() => merge(r.id)} data-testid="inbox-merge">
                        合并
                      </Button>
                    </div>
                  </Card>
                ))}
              </div>
            </section>
          )}

          {/* 待启动的任务 */}
          {data.starts.length > 0 && (
            <section>
              <h2 className="mb-2 text-sm font-medium text-slate-300">待启动的任务（选好机器就开工）</h2>
              <div className="space-y-2">
                {data.starts.map((t) => (
                  <Card key={t.id} className="flex items-center justify-between gap-3 p-4" data-testid="inbox-start">
                    <div className="min-w-0">
                      <p className="truncate text-sm text-white">#{t.id} {t.title}</p>
                      <p className="mt-0.5 truncate text-xs text-slate-500">
                        {t.repo}
                        {t.branch && ` · ${t.branch}`}
                      </p>
                    </div>
                    <Link to={`/tasks?t=${t.id}`}>
                      <Button>去启动 →</Button>
                    </Link>
                  </Card>
                ))}
              </div>
            </section>
          )}
        </div>
      )}
    </div>
  );
}

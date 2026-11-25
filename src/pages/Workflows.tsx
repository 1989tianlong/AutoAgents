import { Fragment, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Card, Empty, Modal, PageHeader, inputClass } from "../components/ui";
import {
  machinesList,
  workflowCancel,
  workflowCreate,
  workflowGate,
  workflowList,
  workflowRetry,
  workflowStart,
  workflowTemplates,
} from "../lib/api";
import { CHAT_ADAPTERS, type Workflow, type WorkflowStep, type WorkflowTemplate } from "../lib/types";

const WF_STATUS: Record<string, { label: string; tone: "slate" | "sky" | "amber" | "emerald" | "rose" }> = {
  pending: { label: "待启动", tone: "slate" },
  running: { label: "进行中", tone: "sky" },
  waiting: { label: "等你决策", tone: "amber" },
  done: { label: "已完成", tone: "emerald" },
  failed: { label: "失败", tone: "rose" },
  cancelled: { label: "已取消", tone: "slate" },
};

const STEP_STATUS: Record<string, { label: string; tone: "slate" | "sky" | "amber" | "emerald" | "rose" }> = {
  pending: { label: "待跑", tone: "slate" },
  running: { label: "进行中", tone: "sky" },
  waiting: { label: "等你", tone: "amber" },
  done: { label: "完成", tone: "emerald" },
  failed: { label: "失败", tone: "rose" },
  skipped: { label: "跳过", tone: "slate" },
};

const KIND_LABEL: Record<string, string> = { task: "任务", session: "会话", gate: "人工门" };

function StatusBadge({ map, status }: { map: typeof WF_STATUS; status: string }) {
  const meta = map[status] ?? { label: status, tone: "slate" as const };
  return <Badge tone={meta.tone}>{meta.label}</Badge>;
}

/** 步骤的产出：任务给分支和 PR，会话给结论文本 */
function StepOutcome({ step }: { step: WorkflowStep }) {  if (step.outcome == null) return null;
  if (step.kind === "task") {
    try {
      const o = JSON.parse(step.outcome);
      return (
        <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-slate-400">
          {o.branch && <code className="rounded bg-slate-800 px-1.5 py-0.5 text-slate-300">{o.branch}</code>}
          {o.prUrl && (
            <a href={o.prUrl} target="_blank" rel="noreferrer" className="text-sky-400 hover:text-sky-300">
              PR →
            </a>
          )}
          {o.taskId != null && (
            <Link to={`/tasks?t=${o.taskId}`} className="text-sky-400 hover:text-sky-300">
              去任务页确认改动 →
            </Link>
          )}
        </div>
      );
    } catch {
      return null;
    }
  }
  if (step.kind === "session") {
    return (
      <details className="mt-1 text-xs text-slate-400">
        <summary className="cursor-pointer select-none text-slate-500 hover:text-slate-300">看结论</summary>
        <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-slate-900 p-2 leading-5 text-slate-300">
          {step.outcome}
        </pre>
      </details>
    );
  }
  return <p className="mt-1 text-xs text-slate-400">{step.outcome}</p>;
}

function WorkflowCard({ wf }: { wf: Workflow }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const gate = wf.steps.find((s) => s.kind === "gate" && s.status === "waiting");
  // 评审门的候选：依赖里带任务号的那些步骤
  const candidates = gate
    ? wf.steps.filter(
        (s) =>
          s.kind === "task" &&
          s.taskId != null &&
          gate.dependsOn.some((d) => d.replace("?", "") === String(s.seq)),
      )
    : [];
  const doneCount = wf.steps.filter((s) => s.status === "done").length;

  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
      qc.invalidateQueries({ queryKey: ["workflows"] });
    } catch (e) {
      window.alert(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="rounded-xl border border-slate-800 bg-slate-900/60 p-5" data-testid="workflow">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="truncate font-medium text-white">{wf.name}</h3>
          <p className="mt-0.5 text-xs text-slate-500">
            {wf.repo}
            {wf.issueNumber != null && ` · Issue #${wf.issueNumber}`}
            {wf.prNumber != null && ` · PR #${wf.prNumber}`}
            {` · ${doneCount}/${wf.steps.length} 步完成`}
          </p>
        </div>
        <StatusBadge map={WF_STATUS} status={wf.status} />
      </div>
      {wf.detail && <p className="mt-1.5 text-xs text-slate-400">{wf.detail}</p>}
      <div className="mt-3 flex flex-wrap items-center gap-1.5">
        {wf.steps.map((s, i) => (
          <Fragment key={s.id}>
            {i > 0 && <span className="text-slate-600">→</span>}
            <span title={s.name} data-testid="step" className="inline-flex items-center gap-1 rounded-md border border-slate-700 bg-slate-900 px-2 py-1 text-xs">
              <StatusBadge map={STEP_STATUS} status={s.status} />
              <span className="text-slate-300">{s.name}</span>
            </span>
          </Fragment>
        ))}
      </div>

      {/* 步骤明细 */}
      <div className="mt-4 space-y-2 border-t border-slate-800 pt-3">
        {wf.steps.map((s) => (
          <div key={s.id} className="text-xs">
            <div className="flex flex-wrap items-center gap-2">
              <span className="rounded bg-slate-800 px-1.5 py-0.5 text-slate-400">{KIND_LABEL[s.kind] ?? s.kind}</span>
              <span className="font-medium text-slate-300">{s.name}</span>
              {s.adapter && <span className="text-slate-500">· {s.adapter}</span>}
              {s.failure && <span className="text-rose-400">{s.failure}</span>}
              {s.detail && !s.failure && <span className="text-slate-500">{s.detail}</span>}
            </div>
            <StepOutcome step={s} />
          </div>
        ))}
      </div>

      {/* 评审门：在候选里选一个 */}
      {gate && candidates.length > 0 && (
        <div className="mt-4 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3">
          <p className="text-xs text-amber-300">人工评审：选一个方案继续（其余方案保持原样，都不往外发）</p>
          <div className="mt-2 flex flex-wrap gap-2">
            {candidates.map((c) => (
              <Button
                key={c.id}
                variant="primary"
                disabled={busy}
                onClick={() => run(() => workflowGate(wf.id, gate.id, c.taskId!))}
              >
                选定「{c.name}」（任务 #{c.taskId}）
              </Button>
            ))}
          </div>
        </div>
      )}

      <div className="mt-4 flex items-center justify-between gap-2">
        <span className="text-xs text-slate-600">建于 {wf.createdAt}</span>
        <div className="flex gap-2">
          {wf.status === "pending" && (
            <Button variant="primary" disabled={busy} onClick={() => run(() => workflowStart(wf.id))}>
              启动
            </Button>
          )}
          {wf.status === "failed" && (
            <Button variant="primary" disabled={busy} onClick={() => run(() => workflowRetry(wf.id))}>
              重跑失败步骤
            </Button>
          )}
          {(wf.status === "running" || wf.status === "waiting") && (
            <Button disabled={busy} onClick={() => run(() => workflowCancel(wf.id))}>
              取消
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

/** 竖着的箭头分隔已经并进步骤徽章的渲染，这里不再需要单独的组件 */

function CreateDialog({ templates, initial, onClose }: { templates: WorkflowTemplate[]; initial?: string; onClose: () => void }) {
  const qc = useQueryClient();
  const creatable = templates.filter((t) => !t.live);
  const [templateId, setTemplateId] = useState(initial ?? creatable[0]?.id ?? "");
  const template = creatable.find((t) => t.id === templateId);
  const [repo, setRepo] = useState("");
  const [issue, setIssue] = useState("");
  const [pr, setPr] = useState("");
  const [instructions, setInstructions] = useState("");
  const [node, setNode] = useState("local");
  const [workdir, setWorkdir] = useState("");
  const [model, setModel] = useState("");
  const [autoPr, setAutoPr] = useState(false);
  const [adapters, setAdapters] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const machines = useQuery({ queryKey: ["machines"], queryFn: machinesList });
  const machine = machines.data?.find((m) => m.id === node);
  const repoName = repo.split("/")[1] ?? "repo";
  const home = machine?.info?.home;

  const pickTemplate = (t: WorkflowTemplate) => {
    setTemplateId(t.id);
    setAdapters(Object.fromEntries(t.steps.map((s) => [s.name, s.adapter ?? ""])));
  };
  // 换模板：各步的 Agent 换成新模板的建议值（用户改过的也一并换掉）
  useEffect(() => {
    if (template) setAdapters(Object.fromEntries(template.steps.map((s) => [s.name, s.adapter ?? ""])));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [templateId]);

  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      await workflowCreate({
        template: templateId,
        repo: repo.trim(),
        issueNumber: templateId !== "pr-review" && issue.trim() ? Number(issue.trim()) : null,
        prNumber: templateId === "pr-review" && pr.trim() ? Number(pr.trim()) : null,
        instructions: instructions.trim() || null,
        nodeId: node,
        workdir: workdir.trim(),
        model: model.trim() || null,
        autoPr,
        // 按步骤顺序原样传（空的是人工门，后端会忽略）
        adapters: template?.steps.map((s) => adapters[s.name] ?? "") ?? [],
      });
      qc.invalidateQueries({ queryKey: ["workflows"] });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="新建工作流"
      onClose={onClose}
      width="max-w-2xl"
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={busy} onClick={create}>
            创建
          </Button>
        </>
      }
    >
      <div className="max-h-[65vh] space-y-4 overflow-y-auto pr-1">
        <div>
          <label className="text-xs text-slate-400">模板</label>
          <div className="mt-1.5 space-y-1.5">
            {creatable.map((t) => (
              <label key={t.id} className="flex cursor-pointer items-start gap-2 rounded-lg border border-slate-800 p-2.5 hover:border-slate-700" data-testid="template-option">
                <input
                  type="radio"
                  name="wf-template"
                  checked={templateId === t.id}
                  onChange={() => pickTemplate(t)}
                  className="mt-0.5"
                />
                <span>
                  <span className="text-sm text-slate-200">{t.name}</span>
                  <span className="ml-2 text-xs text-slate-500">{t.desc}</span>
                </span>
              </label>
            ))}
          </div>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="text-xs text-slate-400">仓库（owner/repo）</span>
            <input value={repo} onChange={(e) => setRepo(e.currentTarget.value)} placeholder="owner/repo" className={`${inputClass} mt-1 font-mono`} data-testid="wf-repo" />
          </label>
          <label className="block">
            <span className="text-xs text-slate-400">Issue 号（没有就填下面的说明）</span>
            <input value={issue} onChange={(e) => setIssue(e.currentTarget.value)} placeholder="比如 7" className={`${inputClass} mt-1`} data-testid="wf-issue" />
          </label>
        </div>
        {template?.needsPr && (
          <label className="block">
            <span className="text-xs text-slate-400">PR 号</span>
            <input value={pr} onChange={(e) => setPr(e.currentTarget.value)} placeholder="比如 42" className={`${inputClass} mt-1`} data-testid="wf-pr" />
          </label>
        )}
        <label className="block">
          <span className="text-xs text-slate-400">需求说明（没有 Issue 时用它，原样交给 Agent）</span>
          <textarea value={instructions} onChange={(e) => setInstructions(e.currentTarget.value)} rows={2} className={`${inputClass} mt-1`} data-testid="wf-instructions" />
        </label>
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="text-xs text-slate-400">在哪台机器上做</span>
            <select value={node} onChange={(e) => setNode(e.currentTarget.value)} className={`${inputClass} mt-1`}>
              <option value="local">本机</option>
              {machines.data?.filter((m) => !m.local).map((m) => (
                <option key={m.id} value={m.id} disabled={!m.online}>
                  {m.name}{m.online ? "" : "（离线）"}
                </option>
              ))}
            </select>
          </label>
          <label className="block">
            <span className="text-xs text-slate-400">仓库在机器上的位置</span>
            <input value={workdir} onChange={(e) => setWorkdir(e.currentTarget.value)} spellCheck={false} placeholder={home ? `${home}/code/${repoName}` : ""} className={`${inputClass} mt-1 font-mono`} data-testid="wf-workdir" />
          </label>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="text-xs text-slate-400">模型（留空用 Agent 默认）</span>
            <input value={model} onChange={(e) => setModel(e.currentTarget.value)} placeholder="可选" className={`${inputClass} mt-1`} />
          </label>
          <label className="mt-5 flex items-center gap-2 text-xs text-slate-300">
            <input type="checkbox" checked={autoPr} onChange={(e) => setAutoPr(e.currentTarget.checked)} />
            任务做完直接提交并开 PR（默认停下等你确认）
          </label>
        </div>
        <div>
          <span className="text-xs text-slate-400">每一步用哪个 Agent</span>
          <div className="mt-1.5 grid grid-cols-2 gap-2">
            {template?.steps.map((s) => (
              <label key={s.name} className="flex items-center gap-2 text-xs">
                <span className="w-28 shrink-0 truncate text-slate-300">{s.name}</span>
                {s.kind === "gate" ? (
                  <span className="text-slate-500">人工操作</span>
                ) : (
                  <select
                    value={adapters[s.name] ?? s.adapter ?? ""}
                    onChange={(e) => setAdapters((prev) => ({ ...prev, [s.name]: e.currentTarget.value }))}
                    className={inputClass}
                  >
                    {CHAT_ADAPTERS.map((a) => (
                      <option key={a} value={a}>{a}</option>
                    ))}
                  </select>
                )}
              </label>
            ))}
          </div>
        </div>
        {error && <p className="text-xs text-rose-400">{error}</p>}
      </div>
    </Modal>
  );
}

export default function Workflows() {
  const [dialog, setDialog] = useState<string | null>(null);
  const templates = useQuery({ queryKey: ["wfTemplates"], queryFn: workflowTemplates });
  const workflows = useQuery({
    queryKey: ["workflows"],
    queryFn: workflowList,
    // 有还在跑的，刷新得勤一点
    refetchInterval: (q) =>
      q.state.data?.some((w) => ["pending", "running", "waiting"].includes(w.status)) ? 3000 : 10000,
  });
  const active = workflows.data?.filter((w) => ["pending", "running", "waiting"].includes(w.status)) ?? [];
  const settled = workflows.data?.filter((w) => !["pending", "running", "waiting"].includes(w.status)) ?? [];

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="工作流"
        desc="多 Agent 协同：几个 Agent 按依赖并行干活，人工门处由你择优；改动往外发仍然逐个确认"
        actions={
          <Button variant="primary" onClick={() => setDialog("create")}>
            ＋ 新建工作流
          </Button>
        }
      />

      {/* 模板 */}
      <div className="mb-8 grid grid-cols-1 gap-3 lg:grid-cols-2">
        {templates.data?.map((t) => (
          <Card key={t.id} className="p-4">
            <div className="flex items-center justify-between gap-3">
              <h2 className="font-medium text-white">{t.name}</h2>
              {t.live ? (
                <Link to={t.live} className="text-xs text-emerald-400 hover:text-emerald-300">
                  已上线 · 去部署页 →
                </Link>
              ) : (
                <Button onClick={() => setDialog(t.id)} className="!px-2 !py-1 text-xs">
                  用这个模板
                </Button>
              )}
            </div>
            <p className="mt-1 text-sm text-slate-400">{t.desc}</p>
            <div className="mt-3 flex flex-wrap items-center gap-1.5">
              {t.steps.map((s, i) => (
                <span key={s.name} className="inline-flex items-center gap-1.5">
                  {i > 0 && <span className="text-slate-600">→</span>}
                  <span className="rounded-md border border-slate-700 bg-slate-900 px-2 py-1 text-xs text-slate-300">
                    {s.name}
                    {s.adapter && <span className="ml-1 text-slate-500">{s.adapter}</span>}
                  </span>
                </span>
              ))}
            </div>
          </Card>
        ))}
      </div>

      {/* 进行中的 */}
      <h2 className="mb-3 text-sm font-medium text-slate-300">我的工作流</h2>
      {workflows.data == null ? null : workflows.data.length === 0 ? (
        <Empty title="还没有工作流" desc="从上面的模板挑一个：竞争模式让两个 Agent 同时解决一个 Issue，交叉审查让第二个 Agent 挑毛病。">
          <Button variant="primary" onClick={() => setDialog("create")}>
            ＋ 新建工作流
          </Button>
        </Empty>
      ) : (
        <div className="space-y-4">
          {[...active, ...settled].map((wf) => (
            <WorkflowCard key={wf.id} wf={wf} />
          ))}
        </div>
      )}

      {dialog && templates.data && (
        <CreateDialog
          templates={templates.data}
          initial={dialog !== "create" ? dialog : undefined}
          onClose={() => setDialog(null)}
        />
      )}
    </div>
  );
}

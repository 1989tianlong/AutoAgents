import { Fragment, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Badge, Button, Card, Empty, Modal, PageHeader, inputClass } from "../components/ui";
import {
  automationRuleCreate,
  automationRuleDelete,
  automationRuleSetEnabled,
  automationRules,
  automationScan,
  machinesList,
  workflowCancel,
  workflowCreate,
  workflowGate,
  workflowList,
  workflowRetry,
  workflowStart,
  workflowTemplates,
} from "../lib/api";
import {
  CHAT_ADAPTERS,
  type AutomationRule,
  type Workflow,
  type WorkflowStep,
  type WorkflowTemplate,
} from "../lib/types";

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

const RULE_KIND: Record<string, { label: string; desc: string }> = {
  "issue-label": { label: "Issue 打标签", desc: "Issue 打上标签就自动开工" },
  "new-pr": { label: "新 PR", desc: "来了新 PR 自动交叉审查、结论贴回 PR" },
  "pr-check-failed": { label: "PR 检查失败", desc: "PR 的检查红了自动让 Agent 修，修完接着推" },
};

const RULE_TEMPLATE: Record<string, string> = {
  "issue-fix": "修 Issue 流水线",
  race: "竞争模式",
  "pr-review": "PR 审查流水线",
  "ci-fix": "自动修检查",
};

function RuleDialog({ templates, onClose }: { templates: WorkflowTemplate[]; onClose: () => void }) {
  const qc = useQueryClient();
  const [kind, setKind] = useState("issue-label");
  const [name, setName] = useState("");
  const [repo, setRepo] = useState("");
  const [label, setLabel] = useState("");
  // 标签规则可选修 Issue 流水线或竞争模式；新 PR 固定走审查；检查失败不建工作流（直接让原任务修）
  const templateOptions = templates.filter((t) =>
    kind === "issue-label" ? t.id === "issue-fix" || t.id === "race" : t.id === "pr-review",
  );
  const [templateId, setTemplateId] = useState("issue-fix");
  const template = templates.find((t) => t.id === templateId);
  const [node, setNode] = useState("local");
  const [workdir, setWorkdir] = useState("");
  const [model, setModel] = useState("");
  const [autoPr, setAutoPr] = useState(true);
  const [adapters, setAdapters] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const machines = useQuery({ queryKey: ["machines"], queryFn: machinesList });

  useEffect(() => {
    const t = templates.find((t) => t.id === templateId);
    if (t) setAdapters(Object.fromEntries(t.steps.map((s) => [s.name, s.adapter ?? ""])));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [templateId]);

  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      await automationRuleCreate({
        name: name.trim(),
        kind,
        repo: repo.trim(),
        label: kind === "issue-label" ? label.trim() : null,
        // 检查失败不建工作流，后端忽略模板；这里传空免得误会
        template: kind === "pr-check-failed" ? null : templateId,
        adapters: template?.steps.map((s) => adapters[s.name] ?? "") ?? [],
        model: kind === "pr-check-failed" ? null : model.trim() || null,
        // 竞争模式停在人工评审门，自动开 PR 会把两个方案都发出去：只在修 Issue 流水线给
        autoPr: kind === "issue-label" && templateId === "issue-fix" && autoPr,
        nodeId: kind === "pr-check-failed" ? null : node,
        workdir: kind === "pr-check-failed" ? null : workdir.trim() || null,
      });
      qc.invalidateQueries({ queryKey: ["rules"] });
      // 建完立刻看一眼 GitHub，别等下一轮
      await automationScan();
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
      title="新建自动化规则"
      onClose={onClose}
      width="max-w-2xl"
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={busy} onClick={create} data-testid="rule-create-submit">
            建规则并检查
          </Button>
        </>
      }
    >
      <div className="max-h-[65vh] space-y-4 overflow-y-auto pr-1">
        <div>
          <label className="text-xs text-slate-400">盯什么</label>
          <div className="mt-1.5 space-y-1.5">
            {Object.entries(RULE_KIND).map(([id, meta]) => (
              <label key={id} className="flex cursor-pointer items-start gap-2 rounded-lg border border-slate-800 p-2.5 hover:border-slate-700">
                <input
                  type="radio"
                  name="rule-kind"
                  checked={kind === id}
                  onChange={() => {
                    setKind(id);
                    setTemplateId(id === "issue-label" ? "issue-fix" : "pr-review");
                  }}
                  className="mt-0.5"
                />
                <span>
                  <span className="text-sm text-slate-200">{meta.label}</span>
                  <span className="ml-2 text-xs text-slate-500">{meta.desc}</span>
                </span>
              </label>
            ))}
          </div>
        </div>
        <div className="grid grid-cols-2 gap-3">
          <label className="block">
            <span className="text-xs text-slate-400">规则名字</span>
            <input value={name} onChange={(e) => setName(e.currentTarget.value)} placeholder="比如：o/r 的 bug 自动修" className={`${inputClass} mt-1`} data-testid="rule-name" />
          </label>
          <label className="block">
            <span className="text-xs text-slate-400">仓库（owner/repo）</span>
            <input value={repo} onChange={(e) => setRepo(e.currentTarget.value)} placeholder="owner/repo" className={`${inputClass} mt-1 font-mono`} data-testid="rule-repo" />
          </label>
        </div>
        {kind === "issue-label" && (
          <div className="grid grid-cols-2 gap-3">
            <label className="block">
              <span className="text-xs text-slate-400">盯的标签（打上它就触发）</span>
              <input value={label} onChange={(e) => setLabel(e.currentTarget.value)} placeholder="比如 agent-fix" className={`${inputClass} mt-1 font-mono`} data-testid="rule-label" />
            </label>
            <label className="block">
              <span className="text-xs text-slate-400">用哪个流程</span>
              <select
                value={templateId}
                onChange={(e) => setTemplateId(e.currentTarget.value)}
                className={`${inputClass} mt-1`}
                data-testid="rule-template"
              >
                {templateOptions.map((t) => (
                  <option key={t.id} value={t.id}>{t.name}</option>
                ))}
              </select>
            </label>
          </div>
        )}
        {kind === "pr-check-failed" && (
          <p className="rounded-lg border border-slate-800 bg-slate-900 p-3 text-xs leading-5 text-slate-400">
            检查红了不另起炉灶：让原来那个任务的 Agent 在同一个会话里继续修，修完照旧走它自己的收尾
            （开着「直接开 PR」的任务会自动推上去，检查再红会再修，每条规则一天最多十次）。
            机器与 Agent 都沿用任务自己的，这里不用选。
          </p>
        )}
        {kind !== "pr-check-failed" && (
          <>
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
                <span className="text-xs text-slate-400">仓库在机器上的位置（留空沿用这个仓库上次的）</span>
                <input value={workdir} onChange={(e) => setWorkdir(e.currentTarget.value)} spellCheck={false} placeholder="可选" className={`${inputClass} mt-1 font-mono`} data-testid="rule-workdir" />
              </label>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <label className="block">
                <span className="text-xs text-slate-400">模型（留空用 Agent 默认）</span>
                <input value={model} onChange={(e) => setModel(e.currentTarget.value)} placeholder="可选" className={`${inputClass} mt-1`} />
              </label>
              {kind === "issue-label" && templateId === "issue-fix" && (
                <label className="mt-5 flex items-center gap-2 text-xs text-slate-300">
                  <input type="checkbox" checked={autoPr} onChange={(e) => setAutoPr(e.currentTarget.checked)} data-testid="rule-autopr" />
                  修完直接提交开 PR（验收就是在决策中心看 PR、合并）
                </label>
              )}
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
          </>
        )}
        {error && <p className="text-xs text-rose-400" data-testid="rule-error">{error}</p>}
      </div>
    </Modal>
  );
}

function RuleCard({ rule }: { rule: AutomationRule }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const run = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
      qc.invalidateQueries({ queryKey: ["rules"] });
    } catch (e) {
      window.alert(String(e));
    } finally {
      setBusy(false);
    }
  };
  const kind = RULE_KIND[rule.kind] ?? { label: rule.kind, desc: "" };
  return (
    <div className="rounded-xl border border-slate-800 bg-slate-900/60 p-4" data-testid="rule">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h3 className="truncate text-sm font-medium text-white">
            {rule.name}
            <span className="ml-2 text-xs font-normal text-slate-500">{rule.repo}</span>
          </h3>
          <p className="mt-1 text-xs text-slate-400">
            {kind.label}
            {rule.label && `「${rule.label}」`} → {RULE_TEMPLATE[rule.template] ?? rule.template}
            {` · 近 24 小时 ${rule.runsToday} 次`}
          </p>
          {rule.adapters.length > 0 && (
            <p className="mt-1 text-xs text-slate-500">Agents：{rule.adapters.filter(Boolean).join(" / ") || "模板默认"}</p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <Badge tone={rule.enabled ? "emerald" : "slate"}>{rule.enabled ? "生效中" : "已停用"}</Badge>
          <Button className="!px-2 !py-1 text-xs" disabled={busy} onClick={() => run(() => automationRuleSetEnabled(rule.id, !rule.enabled))}>
            {rule.enabled ? "停用" : "启用"}
          </Button>
          <Button variant="danger" className="!px-2 !py-1 text-xs" disabled={busy} onClick={() => run(() => automationRuleDelete(rule.id))}>
            删除
          </Button>
        </div>
      </div>
    </div>
  );
}

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
  const [ruleDialog, setRuleDialog] = useState(false);
  const qc = useQueryClient();
  const templates = useQuery({ queryKey: ["wfTemplates"], queryFn: workflowTemplates });
  const rules = useQuery({ queryKey: ["rules"], queryFn: automationRules, refetchInterval: 15000 });
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

      {/* 自动化规则 */}
      <div className="mb-3 mt-10 flex items-center justify-between gap-3">
        <div>
          <h2 className="text-sm font-medium text-slate-300">自动化规则</h2>
          <p className="mt-0.5 text-xs text-slate-500">盯 GitHub：Issue 打标签自动开工，新 PR 自动交叉审查、结论贴回。一条规则一天最多触发十次</p>
        </div>
        <div className="flex gap-2">
          <Button
            onClick={async () => {
              const n = await automationScan();
              qc.invalidateQueries({ queryKey: ["workflows"] });
              window.alert(n > 0 ? `这次触发了 ${n} 条` : "检查完了，这次没有新的");
            }}
          >
            立即检查
          </Button>
          <Button variant="primary" onClick={() => setRuleDialog(true)}>
            ＋ 建规则
          </Button>
        </div>
      </div>
      {rules.data != null &&
        (rules.data.length === 0 ? (
          <Empty title="还没有自动化规则" desc="建一条：给仓库的 Issue 打上指定标签就自动跑修 Issue 流水线；来了新 PR 自动交叉审查并把结论贴回 PR。" />
        ) : (
          <div className="space-y-2">
            {rules.data.map((r) => (
              <RuleCard key={r.id} rule={r} />
            ))}
          </div>
        ))}

      {ruleDialog && templates.data && <RuleDialog templates={templates.data} onClose={() => setRuleDialog(false)} />}

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

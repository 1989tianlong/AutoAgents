import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  agentModels,
  agentsAll,
  deployForTask,
  githubRepos,
  machinesList,
  repoClone,
  taskAbort,
  taskCreateManual,
  taskDelete,
  taskDiff,
  taskDiffFile,
  taskFix,
  taskGet,
  taskRefresh,
  taskStart,
  taskSubmit,
  tasksList,
} from "../lib/api";
import { CHAT_ADAPTERS } from "../lib/types";
import type { DiffView, GhCheck, Task, TaskStatus } from "../lib/types";
import { fmtAgo } from "../lib/format";
import ModelPicker from "../components/ModelPicker";
import DirPicker from "../components/DirPicker";
import { Badge, Button, Card, Empty, Modal, StatusDot, inputClass } from "../components/ui";
import { DEPLOY_STATE } from "./Deploy";

type Tone = "slate" | "sky" | "amber" | "emerald" | "rose" | "violet";

const STATUS: Record<TaskStatus, { label: string; tone: Tone; hint: string }> = {
  pending: { label: "待启动", tone: "slate", hint: "选好机器、Agent 和仓库在那台机器上的位置就能开始" },
  running: { label: "进行中", tone: "sky", hint: "Agent 正在独立的工作树里干活，可以到会话里旁听、插话" },
  changes: { label: "待确认", tone: "amber", hint: "改动已就绪，等你过目后再提交" },
  reviewing: { label: "审查中", tone: "violet", hint: "PR 已创建，等审查、合并" },
  done: { label: "已完成", tone: "emerald", hint: "PR 已合并" },
  failed: { label: "失败", tone: "rose", hint: "看看原因，可以重试" },
};

const ORDER: TaskStatus[] = ["pending", "running", "changes", "reviewing", "failed", "done"];

const ACTIVE: TaskStatus[] = ["running", "changes", "reviewing"];

/** 改动里的一个文件在 diff 里的标记 */
const CHANGE: Record<string, string> = {
  M: "修改",
  A: "新增",
  D: "删除",
  R: "重命名",
  U: "冲突",
  "?": "新文件",
};

export default function Tasks() {
  const qc = useQueryClient();
  const [params, setParams] = useSearchParams();
  const [filter, setFilter] = useState<TaskStatus | "all">("all");
  const [creating, setCreating] = useState(false);
  const tasks = useQuery({
    queryKey: ["tasks"],
    queryFn: tasksList,
    refetchInterval: (query) =>
      query.state.data?.some((t) => ACTIVE.includes(t.status)) ? 5000 : false,
  });
  const selected = Number(params.get("t")) || null;

  const list = (tasks.data ?? []).filter((t) => filter === "all" || t.status === filter);
  const count = (status: TaskStatus) => tasks.data?.filter((t) => t.status === status).length ?? 0;

  return (
    <div className="flex h-full">
      <aside className="flex w-80 shrink-0 flex-col border-r border-slate-800/80">
        <div className="flex items-start justify-between gap-2 p-4 pb-2">
          <div>
            <h1 className="text-lg font-semibold text-white">任务</h1>
            <p className="mt-0.5 text-xs leading-5 text-slate-500">
              Issue → 独立工作树里让 Agent 实现 → 你确认 → 提交、开 PR → 跟踪 CI
            </p>
          </div>
          <Button variant="primary" className="!px-2 !py-1 !text-xs" onClick={() => setCreating(true)}>
            ＋ 新建
          </Button>
        </div>
        <div className="flex flex-wrap gap-1 px-4 pb-2">
          <Chip label="全部" n={tasks.data?.length ?? 0} on={filter === "all"} onClick={() => setFilter("all")} />
          {ORDER.map((status) => (
            <Chip
              key={status}
              label={STATUS[status].label}
              n={count(status)}
              on={filter === status}
              onClick={() => setFilter(status)}
            />
          ))}
        </div>
        <div className="flex-1 space-y-1 overflow-y-auto px-3 pb-3">
          {list.map((t) => (
            <button
              key={t.id}
              onClick={() => setParams({ t: String(t.id) }, { replace: true })}
              className={`w-full rounded-lg px-3 py-2 text-left transition ${
                selected === t.id ? "bg-slate-800" : "hover:bg-slate-800/60"
              }`}
            >
              <div className="line-clamp-2 text-xs leading-5 text-slate-200">{t.title}</div>
              <div className="mt-1 flex items-center gap-2 text-[10px] text-slate-500">
                <Badge tone={STATUS[t.status].tone}>{STATUS[t.status].label}</Badge>
                <span className="truncate">
                  {t.repo?.split("/")[1]}
                  {t.issueNumber != null && ` #${t.issueNumber}`}
                </span>
                <span className="ml-auto shrink-0">{fmtAgo(t.updatedAt)}</span>
              </div>
            </button>
          ))}
          {tasks.isSuccess && list.length === 0 && (
            <div className="rounded-lg border border-dashed border-slate-800 p-3 text-xs leading-5 text-slate-600">
              {tasks.data.length === 0
                ? "还没有任务。点「新建」直接写一段需求，或到 GitHub 页把 Issue 转成任务。"
                : "这一类里没有任务"}
            </div>
          )}
        </div>
      </aside>

      <section className="min-w-0 flex-1 overflow-y-auto">
        {selected == null ? (
          <div className="p-8">
            <Empty title="选一个任务" desc="左边选一个任务看进度、改动和 PR；或者新建一个">
              <Button variant="primary" onClick={() => setCreating(true)}>
                ＋ 新建任务
              </Button>
            </Empty>
          </div>
        ) : (
          <TaskDetail key={selected} id={selected} />
        )}
      </section>

      {creating && (
        <NewTaskDialog
          onClose={() => setCreating(false)}
          onCreated={(id) => {
            setCreating(false);
            qc.invalidateQueries({ queryKey: ["tasks"] });
            setParams({ t: String(id) }, { replace: true });
          }}
        />
      )}
    </div>
  );
}

function Chip({ label, n, on, onClick }: { label: string; n: number; on: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      className={`rounded-md border px-2 py-0.5 text-xs transition ${
        on ? "border-sky-600 bg-sky-950/50 text-white" : "border-slate-700 text-slate-400 hover:border-sky-600"
      }`}
    >
      {label}
      <span className="ml-1 text-slate-500">{n}</span>
    </button>
  );
}

/** 任务走到了哪一步：0 还没开始；之后是已经走到的步骤序号 */
function reachedStep(task: Task): number {
  const offset = task.autoPr ? 1 : 0;
  switch (task.status) {
    case "pending":
      return 0;
    case "running":
      return 1;
    case "changes":
      return 2;
    case "reviewing":
      return 3 - offset;
    case "done":
      return 9;
    case "failed":
      // 失败在哪一步：已有 PR 的是 PR 之后；有会话的是 Agent 那一步；否则还没开始
      if (task.prNumber != null) return 3 - offset;
      return task.sessionId != null ? 1 : 0;
  }
}

/** 这个任务的分支（或它推上去的提交）对应的部署：预览地址、生产状态。没有登记部署目标时什么都不显示 */
function TaskDeployments({ id, branch }: { id: number; branch: string | null }) {
  const navigate = useNavigate();
  const deps = useQuery({
    queryKey: ["deployForTask", id],
    queryFn: () => deployForTask(id),
    refetchInterval: 15000,
  });
  if (!deps.data?.length) return null;
  return (
    <div className="mt-4 flex flex-wrap items-center gap-2 text-xs">
      <span className="text-slate-500">部署</span>
      {deps.data.slice(0, 4).map((d) => (
        <span
          key={d.id}
          className="inline-flex items-center gap-1.5 rounded-md border border-slate-800 bg-slate-900/60 px-2 py-1"
        >
          <Badge tone={DEPLOY_STATE[d.state].tone}>{DEPLOY_STATE[d.state].label}</Badge>
          <span className="text-slate-500">{d.project}</span>
          <span className="text-slate-500">{d.env === "production" ? "生产" : "预览"}</span>
          {d.health != null && d.health !== "ok" && (
            <span className="text-rose-400" title={d.health}>
              健康异常
            </span>
          )}
          {d.url && (
            <button className="text-sky-400 hover:text-sky-300" onClick={() => openUrl(d.url!)}>
              打开 ↗
            </button>
          )}
          <button
            className="text-slate-500 hover:text-slate-300"
            title={`这个分支：${branch ?? "—"}`}
            onClick={() => navigate(`/deploy?t=${d.targetId}`)}
          >
            详情 →
          </button>
        </span>
      ))}
    </div>
  );
}

function TaskDetail({ id }: { id: number }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [file, setFile] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const task = useQuery({
    queryKey: ["task", id],
    queryFn: () => taskGet(id),
    refetchInterval: (query) => {
      const status = query.state.data?.status;
      return status != null && ACTIVE.includes(status) ? 5000 : false;
    },
  });
  const machines = useQuery({ queryKey: ["machines"], queryFn: machinesList });
  const t = task.data;
  const showDiff = !!t?.worktree && ["running", "changes", "failed"].includes(t.status);
  const diff = useQuery({
    queryKey: ["taskDiff", id],
    queryFn: () => taskDiff(id),
    enabled: showDiff,
    refetchInterval: t?.status === "running" ? 8000 : false,
  });
  // Agent 做完（或者出错了）的那一刻，改动才定型：状态一变就重新读一次
  const status = t?.status;
  useEffect(() => {
    if (status === "changes" || status === "failed") {
      qc.invalidateQueries({ queryKey: ["taskDiff", id] });
    }
  }, [status, qc, id]);
  // 没点具体文件时看第一个
  const current = file ?? diff.data?.entries[0]?.path ?? null;
  const untracked = diff.data?.entries.find((entry) => entry.path === current)?.status === "?";
  const patch = useQuery({
    queryKey: ["taskDiffFile", id, current],
    queryFn: () => taskDiffFile(id, current!, untracked),
    enabled: !!current && !!diff.data,
  });

  /** 做一个动作：成功后刷新任务与列表，失败把原因留在页面上 */
  const act = async (action: () => Promise<unknown>, done?: string) => {
    setNotice(null);
    try {
      await action();
      await qc.invalidateQueries({ queryKey: ["task", id] });
      await qc.invalidateQueries({ queryKey: ["tasks"] });
      if (done) setNotice(done);
    } catch (e) {
      setNotice(String(e));
    }
  };

  const remove = async () => {
    if (
      !window.confirm(
        `删除「${t?.title ?? `任务 #${id}`}」？\n它在这个机器上的工作树和本地分支会一并删掉；已经推上去的分支和 PR 不会动。`,
      )
    )
      return;
    try {
      await taskDelete(id);
      qc.invalidateQueries({ queryKey: ["tasks"] });
      navigate("/tasks", { replace: true });
    } catch (e) {
      setNotice(String(e));
    }
  };

  if (task.isPending) return <div className="p-8 text-sm text-slate-500">读取任务…</div>;
  if (task.isError || !t)
    return <div className="p-8 text-sm text-rose-400">{String(task.error ?? "任务不存在")}</div>;

  const machine = machines.data?.find((m) => m.id === t.nodeId);
  const machineName = t.nodeId == null || t.nodeId === "local" ? "本机" : (machine?.name ?? t.nodeId);
  const steps = t.autoPr
    ? ["准备工作树", "Agent 实现", "PR 与 CI", "合并"]
    : ["准备工作树", "Agent 实现", "人工确认", "PR 与 CI", "合并"];
  const reached = reachedStep(t);
  const failing = t.checks?.filter((check) => check.state === "failure").length ?? 0;
  const meta: [string, string][] = [
    ["仓库", t.repo ?? "—"],
    ["机器", machineName],
    ["Agent", t.adapter ? `${t.adapter}${t.model ? ` · ${t.model}` : ""}` : "—"],
  ];
  if (t.branch) meta.push(["分支", t.baseBranch ? `${t.branch}（基于 ${t.baseBranch}）` : t.branch]);
  if (t.worktree) meta.push(["工作树", t.worktree]);

  return (
    <div className="mx-auto max-w-5xl p-8">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <Badge tone={STATUS[t.status].tone}>{STATUS[t.status].label}</Badge>
            <Badge tone={t.source === "issue" ? "sky" : "slate"}>{t.source === "issue" ? "Issue" : "手动"}</Badge>
            {t.autoPr && <Badge tone="violet">自动提交</Badge>}
            {t.issueUrl && (
              <button onClick={() => openUrl(t.issueUrl!)} className="text-xs text-sky-400 hover:text-sky-300">
                Issue #{t.issueNumber} ↗
              </button>
            )}
          </div>
          <h2 className="mt-2 text-lg font-semibold leading-7 text-white">{t.title}</h2>
          <p className="mt-1 text-xs text-slate-500">{STATUS[t.status].hint}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {t.sessionId != null && (
            <Button onClick={() => navigate(`/sessions?s=${t.sessionId}`)}>查看会话</Button>
          )}
          <Button variant="danger" onClick={remove}>
            删除
          </Button>
        </div>
      </div>

      {/* 走到了哪一步 */}
      <div className="mt-5 flex flex-wrap items-center gap-2">
        {steps.map((step, i) => {
          const state = t.status === "failed" && reached === i ? "failed" : reached > i ? "done" : reached === i ? "now" : "todo";
          return (
            <span
              key={step}
              className={`rounded-md border px-2.5 py-1 text-xs ${
                state === "done"
                  ? "border-emerald-800 bg-emerald-950/30 text-emerald-300"
                  : state === "now"
                    ? "border-sky-600 bg-sky-950/40 text-white"
                    : state === "failed"
                      ? "border-rose-800 bg-rose-950/30 text-rose-300"
                      : "border-slate-700 bg-slate-900 text-slate-500"
              }`}
            >
              {state === "done" ? "✓ " : ""}
              {step}
            </span>
          );
        })}
      </div>

      <dl className="mt-5 grid grid-cols-1 gap-x-8 gap-y-2 text-sm sm:grid-cols-2">
        {meta.map(([label, value]) => (
          <div key={label} className="flex items-baseline gap-3">
            <dt className="w-14 shrink-0 text-xs text-slate-500">{label}</dt>
            <dd className="min-w-0 truncate font-mono text-xs text-slate-300" title={value}>
              {value}
            </dd>
          </div>
        ))}
      </dl>

      <TaskDeployments id={t.id} branch={t.branch} />

      {t.detail && <p className="mt-3 text-xs text-slate-400">{t.detail}</p>}
      {t.failure && (
        <pre className="mt-3 whitespace-pre-wrap break-words rounded-lg border border-rose-800/60 bg-rose-950/30 px-4 py-3 text-xs leading-5 text-rose-300">
          {t.failure}
        </pre>
      )}
      {notice && (
        <pre className="mt-3 whitespace-pre-wrap break-words rounded-lg border border-sky-800/60 bg-sky-950/30 px-4 py-3 text-xs leading-5 text-sky-300">
          {notice}
        </pre>
      )}

      {/* 这一步要做的事 */}
      <div className="mt-5 flex flex-wrap items-center gap-2">
        {(t.status === "pending" || t.status === "failed") && (
          <Button variant="primary" onClick={() => setStarting(true)}>
            {t.status === "failed" ? "重试" : "▶ 启动"}
          </Button>
        )}
        {t.status === "running" && (
          <Button variant="danger" onClick={() => act(() => taskAbort(id))}>
            停止这一轮
          </Button>
        )}
        {t.status === "changes" && (
          <>
            <Button variant="primary" onClick={() => setConfirming(true)}>
              提交并创建{t.prNumber != null ? " / 更新" : ""} PR
            </Button>
            {t.sessionId != null && (
              <Button onClick={() => navigate(`/sessions?s=${t.sessionId}`)}>继续对话（让它再改）</Button>
            )}
          </>
        )}
        {t.status === "reviewing" && (
          <>
            {t.prUrl && (
              <Button onClick={() => openUrl(t.prUrl!)}>
                PR #{t.prNumber} ↗
              </Button>
            )}
            <Button onClick={() => act(() => taskRefresh(id))}>刷新检查</Button>
            {failing > 0 && <Button variant="primary" onClick={() => act(() => taskFix(id))}>让 Agent 修复</Button>}
          </>
        )}
        {t.status === "done" && t.prUrl && (
          <Button onClick={() => openUrl(t.prUrl!)}>PR #{t.prNumber} ↗</Button>
        )}
      </div>

      {/* PR 上的检查 */}
      {t.checks && t.checks.length > 0 && (
        <Card className="mt-5 divide-y divide-slate-800/70">
          <div className="px-4 py-2.5 text-xs font-medium text-slate-400">
            PR 上的检查
            <span className="ml-2 text-slate-600">
              {t.checks.filter((c) => c.state === "success").length} 通过 ·{" "}
              {t.checks.filter((c) => c.state === "pending").length} 进行中 · {failing} 失败
            </span>
          </div>
          {t.checks.map((check) => (
            <CheckRow key={check.name} check={check} />
          ))}
        </Card>
      )}

      {/* 改动 */}
      {showDiff && <DiffBrowser diff={diff.data} file={file} patch={patch.data} onPick={setFile} />}

      {starting && (
        <StartDialog
          task={t}
          onClose={() => setStarting(false)}
          onDone={() => {
            setStarting(false);
            qc.invalidateQueries({ queryKey: ["task", id] });
            qc.invalidateQueries({ queryKey: ["tasks"] });
          }}
        />
      )}
      {confirming && (
        <ConfirmDialog
          task={t}
          changed={diff.data?.entries.length ?? 0}
          onClose={() => setConfirming(false)}
          onConfirm={async () => {
            setConfirming(false);
            await act(() => taskSubmit(id));
          }}
        />
      )}
    </div>
  );
}

function CheckRow({ check }: { check: GhCheck }) {
  const mark = { success: "✓", failure: "✗", pending: "◌", skipped: "–" }[check.state];
  const color = {
    success: "text-emerald-400",
    failure: "text-rose-400",
    pending: "text-sky-400",
    skipped: "text-slate-600",
  }[check.state];
  const body = (
    <>
      <span className={`w-4 shrink-0 font-mono ${color}`}>{mark}</span>
      <span className="min-w-0 flex-1 truncate text-xs text-slate-300">{check.name}</span>
      <span className="shrink-0 text-[10px] text-slate-600">
        {{ success: "通过", failure: "失败", pending: "进行中", skipped: "跳过" }[check.state]}
      </span>
    </>
  );
  return check.url ? (
    <button
      onClick={() => openUrl(check.url!)}
      className="flex w-full items-center gap-2 px-4 py-2 text-left transition hover:bg-slate-800/40"
    >
      {body}
    </button>
  ) : (
    <div className="flex items-center gap-2 px-4 py-2">{body}</div>
  );
}

/** 一行 diff 的颜色 */
function lineClass(line: string): string {
  if (line.startsWith("+++") || line.startsWith("---")) return "text-slate-500";
  if (line.startsWith("@@")) return "text-cyan-400";
  if (line.startsWith("+")) return "text-emerald-300";
  if (line.startsWith("-")) return "text-rose-300";
  return "text-slate-400";
}

function DiffBrowser({
  diff,
  file,
  patch,
  onPick,
}: {
  diff?: DiffView;
  /** 选中的文件；没选时看第一个 */
  file: string | null;
  patch?: string;
  onPick: (path: string) => void;
}) {
  if (!diff) return <div className="mt-5 text-xs text-slate-500">读取改动…</div>;
  if (diff.entries.length === 0)
    return (
      <Card className="mt-5 border-dashed p-4 text-xs text-slate-500">
        工作树里还没有改动{diff.ahead > 0 ? `；已经提交了 ${diff.ahead} 个提交` : ""}。
      </Card>
    );
  const first = file ?? diff.entries[0].path;
  return (
    <div className="mt-5">
      <div className="mb-2 flex items-baseline gap-3 px-1 text-xs text-slate-400">
        <span className="font-medium">改动</span>
        <span className="text-slate-600">{diff.entries.length} 个文件</span>
        {diff.stat && <span className="text-slate-600">{diff.stat}</span>}
        {diff.ahead > 0 && <span className="text-slate-600">已提交 {diff.ahead} 个</span>}
      </div>
      <div className="flex overflow-hidden rounded-xl border border-slate-800">
        <div className="max-h-96 w-60 shrink-0 overflow-y-auto border-r border-slate-800 bg-slate-900/40 py-1">
          {diff.entries.map((entry) => (
            <button
              key={entry.path}
              onClick={() => onPick(entry.path)}
              className={`flex w-full items-center gap-2 px-3 py-1.5 text-left transition hover:bg-slate-800 ${
                entry.path === first ? "bg-slate-800" : ""
              }`}
            >
              <span className="w-8 shrink-0 text-[10px] text-slate-500">{CHANGE[entry.status] ?? entry.status}</span>
              <span className="truncate font-mono text-xs text-slate-300" title={entry.path}>
                {entry.path}
              </span>
            </button>
          ))}
        </div>
        <pre className="max-h-96 min-w-0 flex-1 overflow-auto bg-slate-950/60 p-3 font-mono text-[11px] leading-4">
          {patch == null
            ? "读取中…"
            : patch.split("\n").map((line, i) => (
                <div key={i} className={lineClass(line)}>
                  {line || " "}
                </div>
              ))}
        </pre>
      </div>
    </div>
  );
}

/** 提交之前的确认：说清楚会往外发什么 */
function ConfirmDialog({
  task,
  changed,
  onClose,
  onConfirm,
}: {
  task: Task;
  changed: number;
  onClose: () => void;
  onConfirm: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  return (
    <Modal
      title={task.prNumber != null ? `把新的提交推到 PR #${task.prNumber}` : "提交并创建 PR"}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>再看看</Button>
          <Button
            variant="primary"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                await onConfirm();
              } finally {
                setBusy(false);
              }
            }}
          >
            {busy ? "处理中…" : "确认提交"}
          </Button>
        </>
      }
    >
      <ul className="space-y-1.5 text-sm leading-6 text-slate-300">
        <li>把工作树里的 {changed} 个文件提交到分支 <code>{task.branch}</code>。</li>
        <li>
          {task.prNumber != null ? (
            <>推送到远端，已有的 PR #{task.prNumber} 会更新。</>
          ) : (
            <>推送到远端，并向 <code>{task.baseBranch}</code> 开一个 PR。</>
          )}
        </li>
        <li className="text-xs text-slate-500">合并 PR 之前，仓库的默认分支不会有任何变化。</li>
      </ul>
    </Modal>
  );
}

/** 启动任务：选机器、Agent、仓库在那台机器上的位置 */
function StartDialog({ task, onClose, onDone }: { task: Task; onClose: () => void; onDone: () => void }) {
  const machines = useQuery({ queryKey: ["machines"], queryFn: machinesList });
  const agents = useQuery({ queryKey: ["agentsAll"], queryFn: agentsAll });
  const repos = useQuery({ queryKey: ["ghRepos"], queryFn: githubRepos, retry: false });
  const repoName = task.repo?.split("/")[1] ?? "repo";
  const [node, setNode] = useState(task.nodeId ?? "local");
  const [adapter, setAdapter] = useState(task.adapter ?? "");
  const [model, setModel] = useState(task.model ?? "");
  const [workdir, setWorkdir] = useState("");
  const [base, setBase] = useState(task.baseBranch ?? "");
  const [autoPr, setAutoPr] = useState(task.autoPr);
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState<"start" | "clone" | null>(null);
  const [error, setError] = useState<string | null>(null);

  const machine = machines.data?.find((m) => m.id === node);
  const home = machine?.info?.home;
  const online = machine?.online ?? node === "local";
  const available = agents.data?.filter(
    (a) => a.nodeId === node && a.kind === "cli" && (CHAT_ADAPTERS as readonly string[]).includes(a.adapter),
  );
  // 换了机器：Agent、目录跟着换
  useEffect(() => {
    setAdapter(task.nodeId === node && task.adapter ? task.adapter : (available?.[0]?.adapter ?? ""));
    setWorkdir(task.nodeId === node && task.workdir ? task.workdir : home ? `${home}/code/${repoName}` : "");
    setModel(task.nodeId === node ? task.model ?? "" : "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [node, home, available?.length]);
  const models = useQuery({
    queryKey: ["agentModels", adapter, node],
    queryFn: () => agentModels(adapter, node),
    staleTime: 60_000,
  });
  const ssh = repos.data?.find((r) => r.fullName === task.repo)?.sshUrl ?? null;
  const defaultBranch = repos.data?.find((r) => r.fullName === task.repo)?.defaultBranch ?? null;

  const clone = async () => {
    if (!ssh) return;
    setBusy("clone");
    setError(null);
    try {
      const dir = await repoClone(node, ssh, workdir.trim());
      setWorkdir(dir);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  const start = async () => {
    setBusy("start");
    setError(null);
    try {
      await taskStart({
        id: task.id,
        nodeId: node,
        adapter,
        model: model.trim() || null,
        workdir: workdir.trim(),
        baseBranch: base.trim() || null,
        autoPr,
      });
      onDone();
    } catch (e) {
      // 任务本身已经记下了原因；这里只是让人当场看到
      setError(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Modal
      title={task.sessionId != null ? "重新启动任务" : "启动任务"}
      onClose={onClose}
      width="max-w-2xl"
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="primary"
            disabled={busy != null || !online || !adapter || !workdir.trim()}
            onClick={start}
          >
            {busy === "start" ? "启动中…" : "启动"}
          </Button>
        </>
      }
    >
      <div>
        <div className="mb-1.5 text-xs text-slate-500">机器</div>
        <div className="flex flex-wrap gap-2">
          {machines.data?.map((m) => (
            <button
              key={m.id}
              onClick={() => setNode(m.id)}
              disabled={!m.online}
              title={m.online ? undefined : (m.status?.error ?? "不在线")}
              className={`flex items-center gap-2 rounded-lg border px-3 py-1.5 text-sm transition disabled:cursor-not-allowed disabled:opacity-40 ${
                node === m.id
                  ? "border-sky-600 bg-sky-950/50 text-white"
                  : "border-slate-700 text-slate-300 hover:border-sky-600"
              }`}
            >
              <StatusDot online={m.online} />
              {m.name}
            </button>
          ))}
        </div>
      </div>

      <div className="mt-4">
        <div className="mb-1.5 text-xs text-slate-500">Agent</div>
        <div className="flex flex-wrap gap-2">
          {available?.map((a) => (
            <button
              key={a.id}
              onClick={() => {
                setAdapter(a.adapter);
                setModel("");
              }}
              className={`rounded-lg border px-3 py-1.5 text-sm transition ${
                adapter === a.adapter
                  ? "border-sky-600 bg-sky-950/50 text-white"
                  : "border-slate-700 text-slate-300 hover:border-sky-600"
              }`}
            >
              {a.name}
            </button>
          ))}
          {agents.data && available?.length === 0 && (
            <span className="text-xs text-slate-500">这台机器上没有可以干活的 CLI Agent</span>
          )}
        </div>
      </div>

      <div className="mt-4">
        <div className="mb-1.5 text-xs text-slate-500">模型</div>
        <ModelPicker
          options={models.data?.options ?? []}
          custom={models.data?.custom ?? []}
          defaultModel={models.data?.default ?? null}
          value={model}
          onChange={setModel}
        />
      </div>

      <div className="mt-4">
        <div className="mb-1.5 flex items-center justify-between text-xs text-slate-500">
          <span>仓库在这台机器上的位置</span>
          {ssh && (
            <button
              onClick={clone}
              disabled={busy != null || !workdir.trim() || !online}
              className="text-sky-400 hover:text-sky-300 disabled:opacity-40"
            >
              {busy === "clone" ? "克隆中…" : "这台机器上还没有？用 SSH 克隆到这个位置"}
            </button>
          )}
        </div>
        <div className="flex gap-2">
          <input
            value={workdir}
            onChange={(e) => setWorkdir(e.currentTarget.value)}
            spellCheck={false}
            placeholder={home ? `${home}/code/${repoName}` : ""}
            className={`${inputClass} min-w-0 flex-1 font-mono`}
          />
          <button
            onClick={() => setPicking(true)}
            disabled={!home}
            title={home ? "在那台机器上浏览目录" : "连上那台机器之后才能浏览"}
            className="shrink-0 whitespace-nowrap rounded-lg border border-slate-700 px-3 py-2 text-xs text-slate-300 transition hover:border-sky-600 hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
          >
            浏览…
          </button>
        </div>
        <p className="mt-1.5 text-[11px] leading-5 text-slate-600">
          要是那个仓库的一份克隆。Agent 在旁边的 <code>{repoName}-wt/</code> 里干活，不会动这份代码。
        </p>
      </div>

      <div className="mt-4">
        <div className="mb-1.5 text-xs text-slate-500">基础分支</div>
        <input
          value={base}
          onChange={(e) => setBase(e.currentTarget.value)}
          spellCheck={false}
          placeholder={defaultBranch ? `留空用仓库的默认分支（${defaultBranch}）` : "留空用仓库的默认分支"}
          className={`${inputClass} font-mono`}
        />
      </div>

      <label className="mt-4 flex items-start gap-2 text-xs leading-5 text-slate-400">
        <input
          type="checkbox"
          checked={autoPr}
          onChange={(e) => setAutoPr(e.currentTarget.checked)}
          className="mt-0.5 accent-sky-600"
        />
        做完不用我确认，直接提交并创建 PR（PR 仍然要人合并）
      </label>

      {error && <p className="mt-4 whitespace-pre-wrap break-words text-xs text-rose-400">{error}</p>}

      {picking && home && (
        <DirPicker
          node={node}
          machineName={machine?.name ?? "本机"}
          start={workdir.trim() || home}
          onPick={(path) => {
            setWorkdir(path);
            setPicking(false);
          }}
          onClose={() => setPicking(false)}
        />
      )}
    </Modal>
  );
}

/** 直接写一段需求建任务（不经过 Issue） */
function NewTaskDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (id: number) => void }) {
  const repos = useQuery({ queryKey: ["ghRepos"], queryFn: githubRepos, retry: false });
  const [title, setTitle] = useState("");
  const [repo, setRepo] = useState(() => localStorage.getItem("aa.repo") ?? "");
  const [instructions, setInstructions] = useState("");
  const [error, setError] = useState<string | null>(null);
  const create = useMutation({
    mutationFn: () => taskCreateManual({ title, repo, instructions }),
    onSuccess: (task) => {
      localStorage.setItem("aa.repo", repo);
      onCreated(task.id);
    },
    onError: (e) => setError(String(e)),
  });

  return (
    <Modal
      title="新建任务"
      onClose={onClose}
      width="max-w-2xl"
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="primary"
            disabled={create.isPending || !title.trim() || !repo || !instructions.trim()}
            onClick={() => create.mutate()}
          >
            {create.isPending ? "创建中…" : "创建"}
          </Button>
        </>
      }
    >
      <div className="mb-1.5 text-xs text-slate-500">标题</div>
      <input
        value={title}
        onChange={(e) => setTitle(e.currentTarget.value)}
        placeholder="一句话说清楚要做什么"
        className={inputClass}
      />
      <div className="mb-1.5 mt-4 text-xs text-slate-500">仓库</div>
      <select value={repo} onChange={(e) => setRepo(e.currentTarget.value)} className={inputClass}>
        <option value="" disabled>
          选择仓库
        </option>
        {repos.data?.map((r) => (
          <option key={r.id} value={r.fullName}>
            {r.fullName}
          </option>
        ))}
      </select>
      {!repos.data && (
        <p className="mt-1.5 text-xs text-slate-500">
          {repos.isError ? String(repos.error) : "读取仓库列表…"}
        </p>
      )}
      <div className="mb-1.5 mt-4 text-xs text-slate-500">要做什么</div>
      <textarea
        value={instructions}
        onChange={(e) => setInstructions(e.currentTarget.value)}
        rows={7}
        placeholder={"这段话会原样交给 Agent，写清楚：\n- 现在是什么样、希望变成什么样\n- 有哪些约束、不要动什么\n- 怎么算做完"}
        className={`${inputClass} resize-y leading-5`}
      />
      {error && <p className="mt-3 text-xs text-rose-400">{error}</p>}
    </Modal>
  );
}

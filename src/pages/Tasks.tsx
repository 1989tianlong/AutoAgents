import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "react-router-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import { taskDelete, taskSetStatus, tasksList } from "../lib/api";
import type { Task, TaskStatus } from "../lib/types";
import { Badge, PageHeader } from "../components/ui";

type Tone = "slate" | "sky" | "amber" | "emerald" | "rose";

const COLUMNS: { key: TaskStatus; label: string; tone: Tone; next?: TaskStatus }[] = [
  { key: "pending", label: "待处理", tone: "slate", next: "running" },
  { key: "running", label: "进行中", tone: "sky", next: "reviewing" },
  { key: "reviewing", label: "待审查", tone: "amber", next: "done" },
  { key: "done", label: "完成", tone: "emerald" },
  { key: "failed", label: "失败", tone: "rose", next: "pending" },
];

const LABEL: Record<TaskStatus, string> = {
  pending: "待处理",
  running: "进行中",
  reviewing: "待审查",
  done: "完成",
  failed: "失败",
};

export default function Tasks() {
  const qc = useQueryClient();
  const tasks = useQuery({ queryKey: ["tasks"], queryFn: tasksList });
  const setStatus = useMutation({
    mutationFn: (p: { id: number; status: string }) => taskSetStatus(p.id, p.status),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["tasks"] }),
  });
  const remove = useMutation({
    mutationFn: (id: number) => taskDelete(id),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["tasks"] }),
  });

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="任务"
        desc="Issue 转任务的执行单元（自动执行引擎将在 M2/M3 接入工作流后开放）"
        actions={
          <Link
            to="/github"
            className="rounded-lg bg-sky-600 px-3 py-1.5 text-sm font-medium text-white transition hover:bg-sky-500"
          >
            从 Issue 创建
          </Link>
        }
      />

      {tasks.isLoading ? (
        <div className="p-8 text-center text-sm text-slate-500">加载任务…</div>
      ) : tasks.isError ? (
        <div className="p-8 text-center text-sm text-rose-400">{String(tasks.error)}</div>
      ) : (
        <div className="grid grid-cols-2 gap-4 md:grid-cols-3 xl:grid-cols-5">
          {COLUMNS.map((c) => {
            const items = tasks.data?.filter((t) => t.status === c.key) ?? [];
            return (
              <div key={c.key}>
                <div className="mb-2 flex items-center justify-between px-1">
                  <Badge tone={c.tone}>{c.label}</Badge>
                  <span className="text-xs text-slate-500">{items.length}</span>
                </div>
                <div className="min-h-40 space-y-2 rounded-xl border border-slate-800/70 bg-slate-900/30 p-2">
                  {items.map((t) => (
                    <TaskCard
                      key={t.id}
                      task={t}
                      onNext={
                        c.next
                          ? () => setStatus.mutate({ id: t.id, status: c.next! })
                          : undefined
                      }
                      nextLabel={c.next ? LABEL[c.next] : undefined}
                      onDelete={() => remove.mutate(t.id)}
                    />
                  ))}
                  {items.length === 0 && c.key === "pending" && tasks.data?.length === 0 && (
                    <div className="rounded-lg border border-dashed border-slate-700 p-3 text-xs leading-5 text-slate-500">
                      还没有任务。到 GitHub 页选择仓库，把 Issue 一键转为任务。
                    </div>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function TaskCard({
  task,
  onNext,
  nextLabel,
  onDelete,
}: {
  task: Task;
  onNext?: () => void;
  nextLabel?: string;
  onDelete: () => void;
}) {
  return (
    <div className="rounded-lg border border-slate-700/70 bg-slate-900/70 p-3">
      <div className="line-clamp-2 text-xs leading-5 text-slate-200">{task.title}</div>
      <div className="mt-2 flex flex-wrap items-center gap-1.5">
        {task.repo && (
          <span className="truncate text-[10px] text-slate-500" title={task.repo}>
            {task.repo.split("/")[1] ?? task.repo}
            {task.issueNumber != null && ` #${task.issueNumber}`}
          </span>
        )}
        {task.issueUrl && (
          <button
            onClick={() => openUrl(task.issueUrl!)}
            className="text-[10px] text-sky-500 hover:text-sky-400"
          >
            查看
          </button>
        )}
      </div>
      {task.branch && (
        <div className="mt-1.5 truncate font-mono text-[10px] text-violet-400/80">
          {task.branch}
        </div>
      )}
      <div className="mt-2 flex items-center justify-between">
        <span className="text-[10px] text-slate-600">{task.updatedAt.slice(5, 16)}</span>
        <div className="flex items-center gap-1.5">
          {onNext && nextLabel && (
            <button
              onClick={onNext}
              className="rounded border border-slate-700 px-1.5 py-0.5 text-[10px] text-slate-400 transition hover:border-sky-600 hover:text-white"
            >
              → {nextLabel}
            </button>
          )}
          <button
            onClick={onDelete}
            title="删除任务"
            className="rounded border border-slate-700 px-1.5 py-0.5 text-[10px] text-slate-500 transition hover:border-rose-600 hover:text-rose-400"
          >
            ✕
          </button>
        </div>
      </div>
    </div>
  );
}

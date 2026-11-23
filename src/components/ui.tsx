import type { ReactNode } from "react";

export function Card({
  className = "",
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  return (
    <div className={`rounded-xl border border-slate-800 bg-slate-900/60 ${className}`}>
      {children}
    </div>
  );
}

export function PageHeader({
  title,
  desc,
  actions,
}: {
  title: string;
  desc?: string;
  actions?: ReactNode;
}) {
  return (
    <div className="mb-6 flex items-end justify-between gap-4">
      <div>
        <h1 className="text-xl font-semibold text-white">{title}</h1>
        {desc && <p className="mt-1 text-sm text-slate-400">{desc}</p>}
      </div>
      {actions}
    </div>
  );
}

const TONES = {
  emerald: "bg-emerald-500/10 text-emerald-400 border-emerald-500/30",
  sky: "bg-sky-500/10 text-sky-400 border-sky-500/30",
  amber: "bg-amber-500/10 text-amber-400 border-amber-500/30",
  rose: "bg-rose-500/10 text-rose-400 border-rose-500/30",
  violet: "bg-violet-500/10 text-violet-400 border-violet-500/30",
  slate: "bg-slate-500/10 text-slate-400 border-slate-500/30",
} as const;

export function Badge({
  tone = "slate",
  children,
}: {
  tone?: keyof typeof TONES;
  children: ReactNode;
}) {
  return (
    <span
      className={`inline-flex items-center rounded-md border px-1.5 py-0.5 text-xs font-medium ${TONES[tone]}`}
    >
      {children}
    </span>
  );
}

export function StatCard({
  label,
  value,
  sub,
  icon,
}: {
  label: string;
  value: ReactNode;
  sub?: string;
  icon?: ReactNode;
}) {
  return (
    <Card className="flex items-center gap-4 p-4">
      {icon && (
        <div className="grid h-10 w-10 shrink-0 place-items-center rounded-lg bg-slate-800 text-slate-300">
          {icon}
        </div>
      )}
      <div className="min-w-0">
        <div className="text-2xl font-semibold text-white">{value}</div>
        <div className="text-xs text-slate-400">
          {label}
          {sub ? ` · ${sub}` : ""}
        </div>
      </div>
    </Card>
  );
}

export function Empty({
  title,
  desc,
  children,
}: {
  title: string;
  desc?: string;
  children?: ReactNode;
}) {
  return (
    <Card className="grid place-items-center border-dashed p-10 text-center">
      <div>
        <div className="text-sm font-medium text-slate-300">{title}</div>
        {desc && (
          <p className="mx-auto mt-1 max-w-sm text-xs leading-5 text-slate-500">{desc}</p>
        )}
        {children && <div className="mt-4">{children}</div>}
      </div>
    </Card>
  );
}

const LEVEL_TONE = { L3: "emerald", L2: "sky", L1: "slate" } as const;
const LEVEL_DESC = {
  L3: "支持流式双向对话",
  L2: "支持单发指令",
  L1: "支持进程生命周期管理",
} as const;

export function LevelBadge({ level }: { level: "L1" | "L2" | "L3" }) {
  return (
    <span title={LEVEL_DESC[level]}>
      <Badge tone={LEVEL_TONE[level]}>{level}</Badge>
    </span>
  );
}

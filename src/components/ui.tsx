import { useEffect, useState, type ReactNode } from "react";

export function Card({
  className = "",
  children,
  ...props
}: {
  className?: string;
  children: ReactNode;
} & React.HTMLAttributes<HTMLDivElement>) {
  return (
    <div className={`rounded-xl border border-slate-800 bg-slate-900/60 ${className}`} {...props}>
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

/** 文本输入框的统一样式 */
export const inputClass =
  "w-full rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 placeholder:text-slate-600 focus:border-sky-600 focus:outline-none";

type ButtonVariant = "primary" | "ghost" | "danger";

const BUTTON_VARIANT = {
  primary: "bg-sky-600 text-white hover:bg-sky-500 border-transparent",
  ghost: "border-slate-700 text-slate-300 hover:border-sky-600 hover:text-white",
  danger: "border-rose-800 text-rose-300 hover:bg-rose-950/40",
} as const;

export function Button({
  variant = "ghost",
  className = "",
  ...props
}: {
  variant?: ButtonVariant;
} & React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      {...props}
      className={`whitespace-nowrap rounded-lg border px-3 py-1.5 text-sm font-medium transition disabled:cursor-not-allowed disabled:opacity-50 ${BUTTON_VARIANT[variant]} ${className}`}
    />
  );
}

/** 对话框：点遮罩或按 Esc 关闭 */
export function Modal({
  title,
  onClose,
  children,
  footer,
  width = "max-w-lg",
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  width?: string;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-slate-950/70 p-6 backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        className={`w-full ${width} rounded-xl border border-slate-800 bg-slate-900 shadow-2xl`}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-slate-800/80 px-5 py-3">
          <h2 className="text-sm font-medium text-white">{title}</h2>
          <button onClick={onClose} className="text-slate-500 hover:text-slate-300">
            ✕
          </button>
        </div>
        <div className="px-5 py-4">{children}</div>
        {footer && (
          <div className="flex justify-end gap-2 border-t border-slate-800/80 px-5 py-3">
            {footer}
          </div>
        )}
      </div>
    </div>
  );
}

/** 一段可以一键复制的命令 */
export function CopyBox({ text, label }: { text: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // WebView 不给剪贴板权限时退回老办法
      const area = document.createElement("textarea");
      area.value = text;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };
  return (
    <div className="rounded-lg border border-slate-700 bg-slate-950">
      {label && <div className="px-3 pt-2 text-[11px] text-slate-500">{label}</div>}
      <div className="flex items-start gap-2 p-3">
        <code className="min-w-0 flex-1 select-all break-all font-mono text-xs leading-5 text-slate-200">
          {text}
        </code>
        <button
          onClick={copy}
          className="shrink-0 rounded-md border border-slate-700 px-2 py-1 text-xs text-slate-300 transition hover:border-sky-600 hover:text-white"
        >
          {copied ? "已复制" : "复制"}
        </button>
      </div>
    </div>
  );
}

/** 在线状态的小圆点 */
export function StatusDot({ online }: { online: boolean }) {
  return online ? (
    <span className="relative flex h-2.5 w-2.5">
      <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-500 opacity-60" />
      <span className="relative inline-flex h-2.5 w-2.5 rounded-full bg-emerald-500" />
    </span>
  ) : (
    <span className="inline-flex h-2.5 w-2.5 rounded-full bg-slate-600" />
  );
}

/** 一条资源占用条 */
export function Meter({ label, value, text }: { label: string; value: number; text: string }) {
  const tone = value > 0.9 ? "bg-rose-500" : value > 0.7 ? "bg-amber-500" : "bg-sky-500";
  return (
    <div>
      <div className="flex justify-between text-[11px] text-slate-500">
        <span>{label}</span>
        <span className="font-mono">{text}</span>
      </div>
      <div className="mt-1 h-1.5 overflow-hidden rounded-full bg-slate-800">
        <div
          className={`h-full rounded-full ${tone} transition-all`}
          style={{ width: `${Math.min(100, Math.max(0, value * 100))}%` }}
        />
      </div>
    </div>
  );
}

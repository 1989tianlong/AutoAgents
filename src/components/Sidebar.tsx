import { NavLink } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import type { SVGProps } from "react";
import { getAppInfo, inboxList } from "../lib/api";

type IconProps = SVGProps<SVGSVGElement>;

const base = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.8,
  strokeLinecap: "round",
  strokeLinejoin: "round",
} as const;

const IcGrid = (p: IconProps) => (
  <svg {...base} {...p}>
    <rect x="3" y="3" width="7" height="7" rx="1.5" />
    <rect x="14" y="3" width="7" height="7" rx="1.5" />
    <rect x="3" y="14" width="7" height="7" rx="1.5" />
    <rect x="14" y="14" width="7" height="7" rx="1.5" />
  </svg>
);

const IcMonitor = (p: IconProps) => (
  <svg {...base} {...p}>
    <rect x="2" y="3" width="20" height="14" rx="2" />
    <path d="M8 21h8M12 17v4" />
  </svg>
);

const IcBot = (p: IconProps) => (
  <svg {...base} {...p}>
    <rect x="4" y="8" width="16" height="12" rx="2" />
    <path d="M12 8V4M9 4h6" />
    <path d="M9 13v2M15 13v2" />
  </svg>
);

const IcKanban = (p: IconProps) => (
  <svg {...base} {...p}>
    <rect x="3" y="3" width="18" height="18" rx="2" />
    <path d="M8 7v10M12 7v6M16 7v8" />
  </svg>
);

const IcInbox = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="M22 12h-6l-2 3h-4l-2-3H2" />
    <path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z" />
  </svg>
);

const IcBranch = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="M6 3v15" />
    <circle cx="18" cy="6" r="3" />
    <circle cx="6" cy="18" r="3" />
    <path d="M18 9a9 9 0 0 1-9 9" />
  </svg>
);

const IcGithub = (p: IconProps) => (
  <svg viewBox="0 0 24 24" fill="currentColor" {...p}>
    <path d="M12 .5C5.65.5.5 5.65.5 12c0 5.08 3.29 9.39 7.86 10.91.58.1.79-.25.79-.55 0-.27-.01-1.17-.02-2.12-3.2.7-3.88-1.36-3.88-1.36-.52-1.33-1.28-1.68-1.28-1.68-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.18 1.76 1.18 1.03 1.76 2.7 1.25 3.36.96.1-.75.4-1.25.72-1.54-2.55-.29-5.24-1.28-5.24-5.69 0-1.26.45-2.28 1.18-3.09-.12-.29-.51-1.46.11-3.05 0 0 .96-.31 3.15 1.18a10.9 10.9 0 0 1 5.74 0c2.19-1.49 3.15-1.18 3.15-1.18.62 1.59.23 2.76.11 3.05.73.81 1.18 1.83 1.18 3.09 0 4.42-2.7 5.39-5.26 5.68.41.35.77 1.05.77 2.12 0 1.53-.01 2.76-.01 3.14 0 .3.2.66.8.55A11.5 11.5 0 0 0 23.5 12C23.5 5.65 18.35.5 12 .5z" />
  </svg>
);

const IcRocket = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="M4.5 16.5c-1.5 1.26-2 5-2 5s3.74-.5 5-2c.71-.84.7-2.13-.09-2.91a2.18 2.18 0 0 0-2.91-.09z" />
    <path d="m12 15-3-3a22 22 0 0 1 2-3.95A12.88 12.88 0 0 1 22 2c0 2.72-.78 7.5-6 11a22.35 22.35 0 0 1-4 2z" />
    <path d="M9 12H4s.55-3.03 2-4c1.62-1.08 5 0 5 0M12 15v5s3.03-.55 4-2c1.08-1.62 0-5 0-5" />
  </svg>
);

const IcTerminal = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="m4 17 6-6-6-6" />
    <path d="M12 19h8" />
  </svg>
);

const IcPulse = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="M22 12h-4l-3 9L9 3l-3 9H2" />
  </svg>
);

const IcSliders = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="M21 4h-7M10 4H3M21 12h-9M8 12H3M21 20h-5M12 20H3" />
    <path d="M14 2v4M8 10v4M16 18v4" />
  </svg>
);

const NAV = [
  { to: "/", label: "仪表盘", Icon: IcGrid },
  { to: "/inbox", label: "决策", Icon: IcInbox },
  { to: "/machines", label: "机器", Icon: IcMonitor },
  { to: "/agents", label: "Agents", Icon: IcBot },
  { to: "/tasks", label: "任务", Icon: IcKanban },
  { to: "/workflows", label: "工作流", Icon: IcBranch },
  { to: "/github", label: "GitHub", Icon: IcGithub },
  { to: "/deploy", label: "部署", Icon: IcRocket },
  { to: "/sessions", label: "会话", Icon: IcTerminal },
  { to: "/activity", label: "活动", Icon: IcPulse },
  { to: "/settings", label: "设置", Icon: IcSliders },
];

export default function Sidebar() {
  const { data: info } = useQuery({ queryKey: ["appInfo"], queryFn: getAppInfo, staleTime: Infinity });
  // 待决策的事有多少：决策中心有数就亮个角标，不用一页页翻
  const inbox = useQuery({ queryKey: ["inbox"], queryFn: inboxList, refetchInterval: 10000 });
  const pending =
    (inbox.data?.gates.length ?? 0) +
    (inbox.data?.changes.length ?? 0) +
    (inbox.data?.starts.length ?? 0) +
    (inbox.data?.reviews.length ?? 0);
  return (
    <aside className="flex w-56 shrink-0 flex-col border-r border-slate-800/80 bg-slate-900/40">
      <div className="flex items-center gap-3 px-5 py-5">
        <div className="grid h-9 w-9 place-items-center rounded-xl bg-gradient-to-br from-sky-500 to-violet-600 text-sm font-bold text-white">
          AA
        </div>
        <div>
          <div className="text-sm font-semibold text-white">AutoAgents</div>
          <div className="text-xs text-slate-500">多机 Agent 工作台</div>
        </div>
      </div>

      <nav className="flex-1 space-y-0.5 px-3 py-2">
        {NAV.map(({ to, label, Icon }) => (
          <NavLink
            key={to}
            to={to}
            end={to === "/"}
            className={({ isActive }) =>
              `flex items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition ${
                isActive
                  ? "bg-slate-800 text-white"
                  : "text-slate-400 hover:bg-slate-800/60 hover:text-slate-200"
              }`
            }
          >
            <Icon className="h-4.5 w-4.5" />
            {label}
            {to === "/inbox" && pending > 0 && (
              <span className="ml-auto rounded-full bg-amber-500/20 px-1.5 py-0.5 text-[11px] font-semibold text-amber-300" data-testid="inbox-badge">
                {pending}
              </span>
            )}
          </NavLink>
        ))}
      </nav>

      <div className="px-5 py-4 text-xs leading-5 text-slate-500">
        <div>AutoAgents v{info?.version ?? "…"}</div>
        <div>GPL-3.0 · 端到端加密</div>
      </div>
    </aside>
  );
}

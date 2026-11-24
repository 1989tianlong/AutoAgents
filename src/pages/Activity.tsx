import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "react-router-dom";
import { eventsClear, eventsCounts, eventsList } from "../lib/api";
import { KIND_GROUPS, kindLabel, kindTone } from "../lib/events";
import { fmtTime } from "../lib/format";
import type { AppEvent } from "../lib/types";
import { Badge, Button, Card, Empty, PageHeader } from "../components/ui";

const PAGE = 100;

export default function Activity() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const [group, setGroup] = useState<string | null>(null);
  const [limit, setLimit] = useState(PAGE);
  const [busy, setBusy] = useState(false);

  const events = useQuery({
    queryKey: ["events", group, limit],
    queryFn: () => eventsList(limit, group),
    refetchInterval: 5000,
  });
  const counts = useQuery({ queryKey: ["eventCounts"], queryFn: eventsCounts, refetchInterval: 5000 });
  const total = counts.data?.total ?? 0;

  const pick = (key: string | null) => {
    setGroup(key);
    setLimit(PAGE);
  };

  const clear = async () => {
    if (!window.confirm("清空活动记录？只删事件流水，不影响会话、机器与任务。")) return;
    setBusy(true);
    try {
      await eventsClear(0);
      qc.invalidateQueries({ queryKey: ["events"] });
      qc.invalidateQueries({ queryKey: ["eventCounts"] });
    } catch (e) {
      window.alert(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="活动"
        desc="机器、会话、任务、部署的动态与审计，按时间倒序；点右侧可跳到对应的位置"
        actions={
          <Button variant="danger" disabled={busy || total === 0} onClick={clear}>
            清空记录
          </Button>
        }
      />

      <div className="mb-3 flex flex-wrap gap-1.5">
        {KIND_GROUPS.map((g) => (
          <button
            key={g.label}
            onClick={() => pick(g.key)}
            className={`rounded-lg border px-3 py-1 text-sm transition ${
              group === g.key
                ? "border-sky-600 bg-sky-950/50 text-white"
                : "border-slate-700 text-slate-300 hover:border-sky-600"
            }`}
          >
            {g.label}
            <span className="ml-1.5 text-xs text-slate-500">
              {g.key === null ? total : (counts.data?.byKind[g.key] ?? 0)}
            </span>
          </button>
        ))}
      </div>

      <Card className="divide-y divide-slate-800/70">
        {events.isPending && <div className="p-6 text-sm text-slate-500">读取中…</div>}
        {events.isError && <div className="p-6 text-sm text-rose-400">{String(events.error)}</div>}
        {events.data && events.data.length === 0 && (
          <div className="p-2">
            <Empty title="还没有记录" desc="接入机器、发起会话之后，这里会出现每一步的动态" />
          </div>
        )}
        {events.data?.map((event) => (
          <EventRow key={event.id} event={event} onOpen={navigate} />
        ))}
        {events.data && events.data.length === limit && (
          <div className="p-3 text-center">
            <Button onClick={() => setLimit((n) => n + PAGE)}>加载更多</Button>
          </div>
        )}
      </Card>
    </div>
  );
}

function EventRow({
  event,
  onOpen,
}: {
  event: AppEvent;
  onOpen: (to: string) => void;
}) {
  return (
    <div className="flex items-start gap-3 px-4 py-2.5 text-sm">
      <span className="w-40 shrink-0 pt-0.5 font-mono text-xs text-slate-500">
        {fmtTime(event.ts)}
      </span>
      <div className="w-24 shrink-0 pt-0.5">
        <Badge tone={kindTone(event.kind, event.level)}>{kindLabel(event.kind)}</Badge>
      </div>
      <div className="flex min-w-0 flex-1 items-baseline gap-2.5">
        {event.source && <span className="shrink-0 text-xs text-slate-500">{event.source}</span>}
        <span
          className={`min-w-0 break-words ${
            event.level === "error" ? "text-rose-300" : event.level === "warning" ? "text-amber-300" : "text-slate-300"
          }`}
        >
          {event.message}
        </span>
      </div>
      {event.link && (
        <button
          onClick={() => onOpen(event.link!.replace(/^#/, ""))}
          className="shrink-0 text-xs text-sky-400 hover:text-sky-300"
        >
          查看 →
        </button>
      )}
    </div>
  );
}

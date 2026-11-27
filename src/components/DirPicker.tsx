import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { nodeLs } from "../lib/api";
import { Badge, Button, Modal, inputClass } from "./ui";

function join(parent: string, name: string): string {
  return /[\\/]$/.test(parent) ? `${parent}${name}` : `${parent}/${name}`;
}

/** 在某台机器上逐级浏览目录，选一个作为 Agent 的工作目录 */
export default function DirPicker({
  node,
  machineName,
  start,
  onPick,
  onClose,
}: {
  node: string;
  machineName: string;
  /** 起始目录（那台机器上的绝对路径） */
  start: string;
  onPick: (path: string) => void;
  onClose: () => void;
}) {
  const [path, setPath] = useState(start);
  const [draft, setDraft] = useState(start);
  const ls = useQuery({
    queryKey: ["ls", node, path],
    queryFn: () => nodeLs(node, path),
    retry: false,
    enabled: path.trim().length > 0,
  });

  const go = (next: string) => {
    const trimmed = next.trim();
    if (!trimmed) return;
    setPath(trimmed);
    setDraft(trimmed);
  };

  return (
    <Modal
      title={`选择工作目录 · ${machineName}`}
      onClose={onClose}
      width="max-w-2xl"
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="primary"
            disabled={!path.trim() || ls.isError}
            onClick={() => onPick(path.trim())}
          >
            使用这个目录
          </Button>
        </>
      }
    >
      <div className="flex gap-2">
        <Button
          className="shrink-0"
          disabled={!ls.data?.parent}
          onClick={() => ls.data?.parent && go(ls.data.parent)}
          title="上一级"
        >
          ↰ 上级
        </Button>
        <input
          value={draft}
          onChange={(e) => setDraft(e.currentTarget.value)}
          onKeyDown={(e) => e.key === "Enter" && go(draft)}
          spellCheck={false}
          className={`${inputClass} font-mono`}
        />
        <Button className="shrink-0" disabled={!draft.trim()} onClick={() => go(draft)}>
          转到
        </Button>
      </div>
      <div className="mt-3 h-72 overflow-y-auto rounded-lg border border-slate-800 bg-slate-950/40">
        {ls.isPending && <div className="p-3 text-xs text-slate-500">读取中…</div>}
        {ls.isError && (
          <div className="p-3 text-xs leading-5 text-rose-400">{String(ls.error)}</div>
        )}
        {ls.data?.entries.map((entry) => (
          <button
            key={entry.name}
            onClick={() => go(join(ls.data?.path ?? path, entry.name))}
            className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-xs text-slate-300 transition hover:bg-slate-800"
          >
            <span className="text-slate-500">📁</span>
            <span className="truncate font-mono">{entry.name}</span>
            {entry.git && <Badge tone="sky">git</Badge>}
          </button>
        ))}
        {ls.data && ls.data.entries.length === 0 && (
          <div className="p-3 text-xs text-slate-500">这里没有子目录</div>
        )}
      </div>
      <p className="mt-2 text-[11px] text-slate-600">
        列的是那台机器上的目录（不含隐藏目录）；带 git 标记的是代码仓库。
      </p>
    </Modal>
  );
}

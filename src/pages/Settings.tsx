import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  eventsClear,
  eventsCounts,
  getAppInfo,
  githubStatus,
  installScriptSet,
  relayStatus,
} from "../lib/api";
import type { ReactNode } from "react";
import { Badge, Button, Card, PageHeader, inputClass } from "../components/ui";

export default function Settings() {
  const { data: ghUser } = useQuery({
    queryKey: ["ghStatus"],
    queryFn: githubStatus,
    retry: false,
  });
  const { data: info } = useQuery({ queryKey: ["appInfo"], queryFn: getAppInfo });
  const relay = useQuery({ queryKey: ["relayStatus"], queryFn: relayStatus, retry: false });
  const counts = useQuery({ queryKey: ["eventCounts"], queryFn: eventsCounts });

  return (
    <div className="mx-auto max-w-3xl p-8">
      <PageHeader title="设置" desc="连接、凭据与数据都在这里；凡是不在这里的选项，就是还没有的" />

      <Section title="连接">
        <Row
          label="中继"
          desc={
            relay.data?.relay.configured
              ? `${relay.data.relay.addr}${relay.data.connected ? " · 已连接" : relay.data.message ? ` · ${relay.data.message}` : ""}`
              : "未设置：接入其它网络的机器之前先设一个（机器 → 中继设置）"
          }
          badge={
            !relay.data?.relay.configured ? (
              <Badge tone="slate">未设置</Badge>
            ) : relay.data.connected ? (
              <Badge tone="emerald">已连接</Badge>
            ) : (
              <Badge tone="amber">未连接</Badge>
            )
          }
          action={
            <Link to="/machines" className="text-xs text-sky-400 hover:text-sky-300">
              去设置 →
            </Link>
          }
        />
        <InstallScriptRow
          value={relay.data?.relay.installScript ?? ""}
          enabled={!!relay.data}
        />
      </Section>

      <Section title="GitHub">
        <Row
          label="账号"
          desc={ghUser ? `已连接：${ghUser.login}` : "未连接（在 GitHub 页连接）"}
          badge={ghUser ? <Badge tone="emerald">已连接</Badge> : <Badge tone="slate">未连接</Badge>}
          action={
            <Link to="/github" className="text-xs text-sky-400 hover:text-sky-300">
              去 GitHub 页 →
            </Link>
          }
        />
      </Section>

      <Section title="凭据">
        <Row
          label="存放在哪"
          desc="GitHub Token、中继令牌、与各台机器配对的密钥：全部存系统 Keychain（服务名 com.tianlong.autoagents），不进数据库、不写日志、不回传界面"
        />
        <Row
          label="机器之间的连接"
          desc="端到端加密（每一对控制端与机器一把长期密钥）；在中继上只看得到加密流量。移除机器即作废它的密钥"
        />
      </Section>

      <Section title="数据">
        <ClearEventsRow total={counts.data?.total ?? 0} />
        <Row
          label="对话记录"
          desc="会话与消息存在本机的 SQLite 里（应用数据目录），不会上传到任何地方"
        />
      </Section>

      <Section title="关于">
        <Row
          label="AutoAgents"
          desc={
            info
              ? `v${info.version} · GPL-3.0 · 本机 ${info.os} ${info.arch}`
              : "读取版本信息…"
          }
        />
        <Row
          label="机器侧"
          desc="本机由应用内置的机器侧服务提供；其它机器上由 agentd 提供——两者是同一套实现"
        />
      </Section>
    </div>
  );
}

function ClearEventsRow({ total }: { total: number }) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
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
    <Row
      label="活动记录"
      desc={total > 0 ? `共 ${total} 条` : "还没有记录"}
      action={
        <Button variant="danger" disabled={busy || total === 0} onClick={clear}>
          清空
        </Button>
      }
    />
  );
}

/** 安装脚本地址：默认指向仓库 main 分支；内网或自己维护分支时可以改成别的 */
function InstallScriptRow({ value, enabled }: { value: string; enabled: boolean }) {
  const qc = useQueryClient();
  const [url, setUrl] = useState(value);
  const [state, setState] = useState<"idle" | "saving" | "saved" | "error">("idle");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => setUrl(value), [value]);

  const save = async () => {
    setState("saving");
    setError(null);
    try {
      await installScriptSet(url.trim());
      qc.invalidateQueries({ queryKey: ["relayStatus"] });
      setState("saved");
      window.setTimeout(() => setState("idle"), 1500);
    } catch (e) {
      setError(String(e));
      setState("error");
    }
  };

  return (
    <div className="px-5 py-3.5">
      <div className="flex items-center justify-between gap-4">
        <div>
          <div className="text-sm text-slate-200">agentd 安装脚本地址</div>
          <div className="text-xs text-slate-500">
            「接入新机器」生成的命令从这里下载安装脚本；留空用仓库自带的
          </div>
        </div>
        {state === "saved" && <Badge tone="emerald">已保存</Badge>}
        {state === "error" && <Badge tone="rose">保存失败</Badge>}
      </div>
      <div className="mt-2 flex gap-2">
        <input
          value={url}
          onChange={(e) => {
            setUrl(e.currentTarget.value);
            setState("idle");
          }}
          disabled={!enabled}
          spellCheck={false}
          placeholder="https://raw.githubusercontent.com/1989tianlong/AutoAgents/main/scripts/install-agentd.sh"
          className={`${inputClass} font-mono`}
        />
        <Button
          variant="primary"
          disabled={!enabled || state === "saving" || url.trim() === value}
          onClick={save}
        >
          {state === "saving" ? "保存中…" : "保存"}
        </Button>
      </div>
      {error && <p className="mt-1.5 text-xs text-rose-400">{error}</p>}
    </div>
  );
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <Card className="mt-4 divide-y divide-slate-800/70">
      <div className="px-5 py-3 text-sm font-medium text-slate-300">{title}</div>
      {children}
    </Card>
  );
}

function Row({
  label,
  desc,
  badge,
  action,
}: {
  label: string;
  desc: string;
  badge?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 px-5 py-3.5">
      <div className="min-w-0">
        <div className="text-sm text-slate-200">{label}</div>
        <div className="mt-0.5 break-all text-xs leading-5 text-slate-500">{desc}</div>
      </div>
      <div className="flex shrink-0 items-center gap-3">
        {badge}
        {action}
      </div>
    </div>
  );
}

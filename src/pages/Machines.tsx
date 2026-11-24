import { useCallback, useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import {
  machinesList,
  nodeAuthorize,
  nodeConnectNow,
  nodeInstallCommand,
  nodeMetrics,
  nodePair,
  nodePairDirect,
  nodeRemove,
  nodeRename,
  relayClear,
  relaySet,
  relayStatus,
} from "../lib/api";
import { fmtAgo, fmtBytes, fmtCountdown, fmtUptime, short } from "../lib/format";
import type { InstallCommand, MachineView } from "../lib/types";
import {
  Badge,
  Button,
  Card,
  CopyBox,
  Meter,
  Modal,
  PageHeader,
  StatusDot,
  inputClass,
} from "../components/ui";

type Dialog =
  | { kind: "add" }
  | { kind: "relay" }
  | { kind: "rename"; machine: MachineView }
  | { kind: "authorize"; machine: MachineView }
  | { kind: "pair"; nodeId: string; name: string }
  | null;

export default function Machines() {
  const navigate = useNavigate();
  const [params] = useSearchParams();
  const [dialog, setDialog] = useState<Dialog>(params.get("add") ? { kind: "add" } : null);

  const machines = useQuery({
    queryKey: ["machines"],
    queryFn: machinesList,
    refetchInterval: 15000,
  });
  const relay = useQuery({
    queryKey: ["relayStatus"],
    queryFn: relayStatus,
    refetchInterval: 5000,
    retry: false,
  });

  const closeDialog = useCallback(() => setDialog(null), []);

  const list = machines.data ?? [];
  const online = list.filter((m) => m.online).length;
  const agents = list.reduce((sum, m) => sum + m.agentCount, 0);
  const status = relay.data;

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="机器"
        desc="工作电脑、家里的电脑、服务器：装上 agentd 经中继接入，在任何网络下都能管理。连接端到端加密，中继只做转发。"
        actions={
          <>
            <Button onClick={() => setDialog({ kind: "relay" })}>中继设置</Button>
            <Button variant="primary" onClick={() => setDialog({ kind: "add" })}>
              ＋ 接入新机器
            </Button>
          </>
        }
      />

      <div className="mb-4 grid grid-cols-2 gap-4 xl:grid-cols-4">
        <Card className="px-5 py-3">
          <div className="text-xs text-slate-500">机器</div>
          <div className="mt-0.5 text-2xl font-semibold text-white">
            {machines.data ? list.length : "…"}
            <span className="ml-2 text-sm font-normal text-slate-500">在线 {online}</span>
          </div>
        </Card>
        <Card className="px-5 py-3">
          <div className="text-xs text-slate-500">Agent</div>
          <div className="mt-0.5 text-2xl font-semibold text-white">
            {machines.data ? agents : "…"}
            <span className="ml-2 text-sm font-normal text-slate-500">所有机器合计</span>
          </div>
        </Card>
        <Card className="px-5 py-3">
          <div className="text-xs text-slate-500">中继</div>
          <div className="mt-1.5">
            {status ? (
              <Badge tone={!status.relay.configured ? "slate" : status.connected ? "emerald" : "amber"}>
                {!status.relay.configured ? "未设置" : status.connected ? "已连接" : "连接中…"}
              </Badge>
            ) : (
              "…"
            )}
          </div>
        </Card>
        <Card className="px-5 py-3">
          <div className="text-xs text-slate-500">等待接入</div>
          <div className="mt-0.5 text-2xl font-semibold text-white">
            {status ? status.pending.length : "…"}
            <span className="ml-2 text-sm font-normal text-slate-500">份安装命令未用</span>
          </div>
        </Card>
      </div>

      {status?.relay.configured && (
        <Card className="mb-4 px-5 py-4">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
            <span className="text-slate-400">中继</span>
            <span className="font-mono text-xs text-slate-200">{status.relay.addr}</span>
            <Badge tone={status.connected ? "emerald" : "amber"}>
              {status.connected ? "已连接" : "未连接"}
            </Badge>
            {status.message && <span className="text-xs text-amber-400">{status.message}</span>}
            {status.relay.fingerprint && (
              <span
                className="font-mono text-[11px] text-slate-600"
                title={status.relay.fingerprint}
              >
                证书 {short(status.relay.fingerprint.replace("sha256:", ""), 8, 8)}
              </span>
            )}
          </div>

          {status.pending.length > 0 && (
            <div className="mt-3 flex flex-wrap items-center gap-2 border-t border-slate-800/70 pt-3 text-xs">
              <span className="text-slate-500">等待接入：</span>
              {status.pending.map((invite) => (
                <span
                  key={invite.name}
                  className="inline-flex items-center gap-1.5 rounded-md border border-sky-900 bg-sky-950/30 px-2 py-1 text-sky-300"
                >
                  <span className="inline-block h-2 w-2 animate-pulse rounded-full bg-sky-400" />
                  {invite.name}
                  <span className="font-mono text-sky-500">{fmtCountdown(invite.expiresInS)}</span>
                </span>
              ))}
            </div>
          )}

          {status.online.length > 0 && (
            <div className="mt-3 border-t border-slate-800/70 pt-3">
              <div className="mb-2 text-xs text-slate-500">
                中继上的机器
                <span className="ml-2 text-slate-600">
                  {status.online.filter((n) => n.paired).length}/{status.online.length} 已配对
                </span>
              </div>
              <div className="space-y-1">
                {status.online.map((node) => (
                  <div key={node.nodeId} className="flex items-center gap-3 text-sm">
                    <StatusDot online />
                    <span className="text-slate-200">{node.name}</span>
                    <span className="text-xs text-slate-500">{node.os}</span>
                    <span className="text-xs text-slate-600">已连 {fmtUptime((Date.now() - node.sinceMs) / 1000)}</span>
                    <span className="ml-auto">
                      {node.paired ? (
                        <Badge tone="emerald">已配对</Badge>
                      ) : (
                        <Button
                          variant="primary"
                          className="!px-2 !py-0.5 !text-xs"
                          onClick={() => setDialog({ kind: "pair", nodeId: node.nodeId, name: node.name })}
                        >
                          配对
                        </Button>
                      )}
                    </span>
                  </div>
                ))}
              </div>
            </div>
          )}
        </Card>
      )}

      {status && !status.relay.configured && machines.isSuccess && (
        <Card className="mb-4 border-dashed px-5 py-4">
          <div className="text-sm text-slate-300">还没有设置中继</div>
          <p className="mt-1 text-xs leading-5 text-slate-500">
            中继是一台放在公网的小程序（一台 VPS 就够）：各台机器与这台电脑都只需向它发起出站连接，
            在公司网络、家庭路由器后面也能互通。它只验令牌、转发密文，看不到任何内容。
          </p>
          <Button variant="primary" className="mt-3" onClick={() => setDialog({ kind: "relay" })}>
            设置中继
          </Button>
        </Card>
      )}

      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
        {list.map((machine) => (
          <MachineCard
            key={machine.id}
            machine={machine}
            onDialog={setDialog}
            onChat={() => navigate(`/sessions?node=${machine.id}`)}
          />
        ))}
      </div>

      {dialog?.kind === "add" && (
        <AddMachineDialog onClose={closeDialog} onRelay={() => setDialog({ kind: "relay" })} />
      )}
      {dialog?.kind === "relay" && <RelayDialog onClose={closeDialog} />}
      {dialog?.kind === "pair" && (
        <PairDialog nodeId={dialog.nodeId} name={dialog.name} onClose={closeDialog} />
      )}
      {dialog?.kind === "authorize" && (
        <AuthorizeDialog machine={dialog.machine} onClose={closeDialog} />
      )}
      {dialog?.kind === "rename" && (
        <RenameDialog machine={dialog.machine} onClose={closeDialog} />
      )}
    </div>
  );
}

function MachineCard({
  machine,
  onDialog,
  onChat,
}: {
  machine: MachineView;
  onDialog: (d: Dialog) => void;
  onChat: () => void;
}) {
  const qc = useQueryClient();
  const info = machine.info;
  const metrics = useQuery({
    queryKey: ["metrics", machine.id],
    queryFn: () => nodeMetrics(machine.id),
    refetchInterval: 5000,
    enabled: machine.online,
  });

  const reconnect = async () => {
    try {
      await nodeConnectNow(machine.id);
    } catch (e) {
      window.alert(String(e));
    }
    qc.invalidateQueries({ queryKey: ["machines"] });
  };
  const remove = async () => {
    if (
      !window.confirm(
        `移除「${machine.name}」？\n会断开连接并删除配对密钥；那台机器需要重新配对才能再接入。`,
      )
    )
      return;
    try {
      await nodeRemove(machine.id);
    } catch (e) {
      window.alert(String(e));
    }
    qc.invalidateQueries({ queryKey: ["machines"] });
    qc.invalidateQueries({ queryKey: ["relayStatus"] });
  };

  const mem = metrics.data;
  const memTotal = mem?.memTotal ?? info?.memTotal ?? 0;
  const memUsed = mem?.memUsed ?? 0;

  return (
    <Card className="flex flex-col p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="flex min-w-0 items-center gap-3">
          <StatusDot online={machine.online} />
          <div className="min-w-0">
            <div className="truncate text-sm font-medium text-white">{machine.name}</div>
            <div className="truncate text-xs text-slate-500">
              {info ? `${info.os} · ${info.arch}${info.hostname && info.hostname !== machine.name ? ` · ${info.hostname}` : ""}` : "还没有连上过"}
            </div>
          </div>
        </div>
        {machine.local ? (
          <Badge tone="sky">本机</Badge>
        ) : machine.online ? (
          <Badge tone="emerald">在线</Badge>
        ) : (
          <Badge tone="slate">离线</Badge>
        )}
      </div>

      {!machine.online && machine.status?.error && (
        <p className="mt-3 line-clamp-2 text-xs leading-5 text-rose-400" title={machine.status.error}>
          {machine.status.error}
        </p>
      )}

      <dl className="mt-4 space-y-2 text-sm">
        {info && (
          <>
            <Row label="机器侧服务" value={`${info.flavor} v${info.version}`} />
            <Row label="CPU / 内存" value={`${info.cpuCount} 核 · ${fmtBytes(info.memTotal)}`} />
          </>
        )}
        <Row
          label="Agent"
          value={machine.online || machine.local ? `${machine.agentCount} 个` : "—"}
        />
        {machine.online && machine.status?.rttMs != null && (
          <Row label="往返延迟" value={`${machine.status.rttMs} 毫秒`} />
        )}
        {!machine.online && machine.status?.lastSeenMs ? (
          <Row label="最后在线" value={fmtAgo(machine.status.lastSeenMs)} />
        ) : null}
      </dl>

      {machine.online && mem && (
        <div className="mt-4 space-y-3">
          <Meter
            label="CPU"
            value={mem.cpuPercent / 100}
            text={`${mem.cpuPercent.toFixed(0)}%${mem.load1 > 0 ? ` · 负载 ${mem.load1.toFixed(2)}` : ""}`}
          />
          <Meter
            label="内存"
            value={memTotal ? memUsed / memTotal : 0}
            text={`${fmtBytes(memUsed)} / ${fmtBytes(memTotal)}`}
          />
          {mem.runningTurns > 0 && (
            <div className="text-xs text-sky-400">正在运行 {mem.runningTurns} 轮对话</div>
          )}
        </div>
      )}

      <div className="mt-auto flex flex-wrap gap-2 pt-4">
        <Button variant="primary" className="!px-2.5 !py-1 !text-xs" onClick={onChat}>
          💬 会话
        </Button>
        {!machine.local && (
          <>
            <Button className="!px-2.5 !py-1 !text-xs" onClick={() => onDialog({ kind: "rename", machine })}>
              重命名
            </Button>
            <Button
              className="!px-2.5 !py-1 !text-xs"
              disabled={!machine.online}
              onClick={() => onDialog({ kind: "authorize", machine })}
              title="让另一台电脑上的 AutoAgents 也能管理这台机器"
            >
              授权其他设备
            </Button>
            <Button className="!px-2.5 !py-1 !text-xs" onClick={reconnect}>
              立即重连
            </Button>
            <Button variant="danger" className="!px-2.5 !py-1 !text-xs" onClick={remove}>
              移除
            </Button>
          </>
        )}
      </div>
    </Card>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between gap-4">
      <dt className="shrink-0 text-xs text-slate-500">{label}</dt>
      <dd className="truncate text-xs text-slate-300" title={value}>
        {value}
      </dd>
    </div>
  );
}

/** 接入新机器：生成带一次性配对码的安装命令，等它连上中继后自动配对 */
function AddMachineDialog({ onClose, onRelay }: { onClose: () => void; onRelay: () => void }) {
  const qc = useQueryClient();
  const relay = useQuery({
    queryKey: ["relayStatus"],
    queryFn: relayStatus,
    refetchInterval: 3000,
    retry: false,
  });
  const [name, setName] = useState("");
  const [allow, setAllow] = useState("");
  const [direct, setDirect] = useState("");
  const [command, setCommand] = useState<InstallCommand | null>(null);
  const [generatedAt, setGeneratedAt] = useState(0);
  const [now, setNow] = useState(Date.now());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  // 安装命令的倒计时，以及「机器接入成功」的提示
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const left = command ? Math.max(0, command.expiresInS - (now - generatedAt) / 1000) : 0;
  const waiting = command != null && !done && left > 0;
  const paired = relay.data?.online.find((n) => n.name === command?.name && n.paired);
  const pairedId = paired?.nodeId ?? null;
  useEffect(() => {
    if (pairedId && !done) {
      setDone(true);
      qc.invalidateQueries({ queryKey: ["machines"] });
      qc.invalidateQueries({ queryKey: ["agentsAll"] });
    }
  }, [pairedId, done, qc]);
  // 提示一会儿再关。单独放在一个只看 done 的副作用里：
  // 中继状态每几秒刷新一次，跟着它重排的副作用会把这个定时器取消掉
  useEffect(() => {
    if (!done) return;
    const timer = window.setTimeout(onClose, 1800);
    return () => window.clearTimeout(timer);
  }, [done, onClose]);

  const generate = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await nodeInstallCommand(
        name,
        allow
          .split(/\n|,/)
          .map((s) => s.trim())
          .filter(Boolean),
      );
      setCommand(result);
      setGeneratedAt(Date.now());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const pairDirect = async () => {
    setBusy(true);
    setError(null);
    try {
      await nodePairDirect(direct.trim());
      qc.invalidateQueries({ queryKey: ["machines"] });
      qc.invalidateQueries({ queryKey: ["agentsAll"] });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="接入新机器"
      onClose={onClose}
      width="max-w-2xl"
      footer={
        command ? (
          <Button onClick={onClose}>{done ? "完成" : "关闭"}</Button>
        ) : (
          <Button variant="primary" disabled={busy || !name.trim()} onClick={generate}>
            {busy ? "生成中…" : "生成安装命令"}
          </Button>
        )
      }
    >
      {!relay.data?.relay.configured && (
        <div className="mb-4 rounded-lg border border-amber-800/60 bg-amber-950/30 px-4 py-2 text-xs leading-5 text-amber-300">
          还没有设置中继：接入其它网络的机器要先有一个中继。
          <button className="ml-2 underline underline-offset-2" onClick={onRelay}>
            去设置
          </button>
        </div>
      )}

      {!command ? (
        <>
          <label className="mb-1 block text-xs text-slate-500">机器名字</label>
          <input
            value={name}
            onChange={(e) => setName(e.currentTarget.value)}
            placeholder="work-pc / home-mac / server-1"
            className={inputClass}
          />
          <p className="mt-1.5 text-[11px] text-slate-600">
            起一个认得出来的名字；中继上的在线列表、会话、Agent 清单都按它区分。
          </p>

          <label className="mb-1 mt-4 block text-xs text-slate-500">
            允许访问的目录（可选，每行一个）
          </label>
          <textarea
            value={allow}
            onChange={(e) => setAllow(e.currentTarget.value)}
            rows={2}
            placeholder={"~/code\n/opt/apps"}
            spellCheck={false}
            className={`${inputClass} resize-none font-mono`}
          />
          <p className="mt-1.5 text-[11px] text-slate-600">
            留空表示不限制。填了之后，这台机器上的 Agent 只能在这些目录里读写文件、执行命令。
          </p>

          <details className="mt-5 border-t border-slate-800/70 pt-4">
            <summary className="cursor-pointer text-xs text-slate-400">
              那台机器有公网地址？改用直连（不经过中继）
            </summary>
            <p className="mt-2 text-[11px] leading-5 text-slate-600">
              在那台机器上执行 <code className="text-slate-400">agentd pair</code>
              ，把它打印的连接串粘贴到这里：
            </p>
            <div className="mt-2 flex gap-2">
              <input
                value={direct}
                onChange={(e) => setDirect(e.currentTarget.value)}
                placeholder="autoagents://node?addr=wss://…"
                spellCheck={false}
                className={`${inputClass} font-mono`}
              />
              <Button variant="primary" disabled={busy || !direct.trim()} onClick={pairDirect}>
                配对
              </Button>
            </div>
          </details>
        </>
      ) : (
        <>
          <ol className="space-y-2 text-sm leading-6 text-slate-300">
            <li>
              <span className="mr-2 text-slate-500">1.</span>
              在那台机器的终端里执行下面这条命令（需要 <code>curl</code>；会下载 agentd 并安装成系统服务，随开机启动）：
            </li>
          </ol>
          <div className="mt-3">
            <CopyBox text={command.command} />
          </div>
          <div className="mt-4 space-y-2 text-sm leading-6 text-slate-300">
            <p>
              <span className="mr-2 text-slate-500">2.</span>
              {done ? (
                <span className="text-emerald-400">已接入「{command.name}」，正在关闭…</span>
              ) : waiting ? (
                <>
                  等它连上中继，这里会
                  <span className="text-sky-400">自动完成配对</span>
                  <span className="ml-2 inline-flex items-center gap-1.5 text-xs text-slate-500">
                    <span className="inline-block h-3 w-3 animate-spin rounded-full border-2 border-slate-600 border-t-sky-400" />
                    等待接入 · 配对码 {fmtCountdown(left)} 内有效
                  </span>
                </>
              ) : (
                <span className="text-amber-400">
                  配对码已过期。关掉这个窗口重新生成一条安装命令。
                </span>
              )}
            </p>
            <p className="text-[11px] leading-5 text-slate-600">
              配对码只能用一次。配对成功后双方各保存一把长期密钥，之后的连接端到端加密，中继看不到内容；
              在这里「移除」机器，那台机器上的密钥随即作废。
            </p>
          </div>
        </>
      )}
      {error && <p className="mt-4 text-xs text-rose-400">{error}</p>}
    </Modal>
  );
}

/** 与中继上一台在线、尚未配对的机器配对：另一台电脑上的 AutoAgents 给它授权过，或在它上面执行过 agentd pair */
function PairDialog({ nodeId, name, onClose }: { nodeId: string; name: string; onClose: () => void }) {
  const qc = useQueryClient();
  const [code, setCode] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const pair = async () => {
    setBusy(true);
    setError(null);
    try {
      const text = code.trim();
      if (text.startsWith("autoagents://")) await nodePairDirect(text);
      else await nodePair(nodeId, text, name);
      qc.invalidateQueries({ queryKey: ["machines"] });
      qc.invalidateQueries({ queryKey: ["relayStatus"] });
      qc.invalidateQueries({ queryKey: ["agentsAll"] });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={`与「${name}」配对`}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={busy || code.trim().length < 4} onClick={pair}>
            {busy ? "配对中…" : "配对"}
          </Button>
        </>
      }
    >
      <p className="text-xs leading-5 text-slate-400">
        这台机器已经连上中继，但还没有和这台电脑配对。输入它的配对码：由另一台电脑上的 AutoAgents
        （机器 → 授权其他设备）给出，或在它上面执行 <code>agentd pair</code> 得到。
      </p>
      <input
        value={code}
        onChange={(e) => setCode(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && pair()}
        placeholder="XXXX-XXXX-XXXX-XXXX 或 autoagents://node?… 连接串"
        spellCheck={false}
        autoFocus
        className={`${inputClass} mt-3 font-mono`}
      />
      {error && <p className="mt-3 text-xs text-rose-400">{error}</p>}
    </Modal>
  );
}

/** 中继设置：粘贴中继启动时打印的连接串 */
function RelayDialog({ onClose }: { onClose: () => void }) {
  const qc = useQueryClient();
  const relay = useQuery({ queryKey: ["relayStatus"], queryFn: relayStatus, retry: false });
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = relay.data?.relay;
  const configured = !!current?.configured;

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await relaySet({ text: text.trim() || null });
      qc.invalidateQueries({ queryKey: ["relayStatus"] });
      qc.invalidateQueries({ queryKey: ["machines"] });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const clear = async () => {
    if (!window.confirm("清除中继配置？经中继接入的机器会连不上，直到重新设置。")) return;
    await relayClear();
    qc.invalidateQueries({ queryKey: ["relayStatus"] });
    qc.invalidateQueries({ queryKey: ["machines"] });
    onClose();
  };

  return (
    <Modal
      title="中继设置"
      onClose={onClose}
      width="max-w-2xl"
      footer={
        <>
          {configured && (
            <Button variant="danger" className="mr-auto" onClick={clear}>
              清除
            </Button>
          )}
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={busy || !text.trim()} onClick={save}>
            {busy ? "保存中…" : "保存"}
          </Button>
        </>
      }
    >
      {configured && current && (
        <div className="mb-4 rounded-lg border border-slate-800 bg-slate-950/60 px-4 py-3 text-xs leading-5">
          <div className="text-slate-400">
            当前中继 <span className="font-mono text-slate-200">{current.addr}</span>
          </div>
          <div className="mt-1 text-slate-600">
            令牌已保存（不显示）· 证书{" "}
            <span className="font-mono" title={current.fingerprint}>
              {current.fingerprint ? short(current.fingerprint.replace("sha256:", ""), 8, 8) : "由系统校验"}
            </span>
          </div>
        </div>
      )}

      <label className="mb-1 block text-xs text-slate-500">中继连接串</label>
      <textarea
        value={text}
        onChange={(e) => setText(e.currentTarget.value)}
        rows={2}
        spellCheck={false}
        placeholder="autoagents://relay?addr=wss://relay.example.com:7000&token=…&fp=sha256:…"
        className={`${inputClass} resize-none font-mono`}
      />
      <p className="mt-1.5 text-[11px] leading-5 text-slate-600">
        粘贴 autoagents-relay 启动时打印的那一行即可；地址、令牌、证书指纹都带着。令牌存系统 Keychain，不会写进数据库或日志。
      </p>

      <details className="mt-4 border-t border-slate-800/70 pt-4" open={!configured}>
        <summary className="cursor-pointer text-xs text-slate-400">还没有中继？一台有公网地址的服务器就够了</summary>
        <ol className="mt-3 space-y-2 text-xs leading-5 text-slate-400">
          <li>1. 在服务器上安装并启动（Linux / macOS，需要 7000 端口可访问）：</li>
        </ol>
        <div className="mt-2">
          <CopyBox
            text={"curl -fsSL https://raw.githubusercontent.com/1989tianlong/AutoAgents/main/scripts/install-relay.sh | sh\nautoagents-relay --public wss://<公网IP或域名>:7000"}
          />
        </div>
        <ol className="mt-3 space-y-2 text-xs leading-5 text-slate-400" start={2}>
          <li>2. 把它打印的「中继连接串」粘贴到上面，保存。</li>
          <li>3. 点「接入新机器」生成安装命令，到其它机器上执行。</li>
        </ol>
        <p className="mt-3 text-[11px] leading-5 text-slate-600">
          自带 TLS（自签名证书，按指纹钉住），不需要域名；已有域名与证书的话可以加 <code>--no-tls</code>{" "}
          放在 Caddy / Nginx 后面。
        </p>
      </details>
      {error && <p className="mt-4 text-xs text-rose-400">{error}</p>}
    </Modal>
  );
}

/** 让另一台电脑上的 AutoAgents 也能管理这台机器 */
function AuthorizeDialog({ machine, onClose }: { machine: MachineView; onClose: () => void }) {
  const [code, setCode] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    nodeAuthorize(machine.id)
      .then(setCode)
      .catch((e: unknown) => setError(String(e)));
  }, [machine.id]);

  return (
    <Modal title={`授权其他设备 · ${machine.name}`} onClose={onClose} footer={<Button onClick={onClose}>关闭</Button>}>
      {error ? (
        <p className="text-xs text-rose-400">{error}</p>
      ) : !code ? (
        <p className="text-xs text-slate-500">正在让机器开放配对…</p>
      ) : (
        <>
          <p className="text-xs leading-5 text-slate-400">
            在另一台电脑的 AutoAgents 里：先把同一个中继连接串填到「机器 → 中继设置」，
            再到中继上的机器列表里找到「{machine.name}」点「配对」，输入：
          </p>
          <div className="mt-3">
            <CopyBox text={code} />
          </div>
          <p className="mt-3 text-[11px] text-slate-600">配对码 10 分钟内有效，只能用一次。</p>
        </>
      )}
    </Modal>
  );
}

function RenameDialog({ machine, onClose }: { machine: MachineView; onClose: () => void }) {
  const qc = useQueryClient();
  const [name, setName] = useState(machine.name);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await nodeRename(machine.id, name);
      qc.invalidateQueries({ queryKey: ["machines"] });
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title="重命名"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={busy || !name.trim() || name.trim() === machine.name} onClick={save}>
            保存
          </Button>
        </>
      }
    >
      <input
        value={name}
        onChange={(e) => setName(e.currentTarget.value)}
        onKeyDown={(e) => e.key === "Enter" && save()}
        autoFocus
        className={inputClass}
      />
      {error && <p className="mt-3 text-xs text-rose-400">{error}</p>}
    </Modal>
  );
}

import { useEffect, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate, useSearchParams } from "react-router-dom";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  deployCancel,
  deployCheckHealth,
  deployConnect,
  deployDisconnect,
  deployDiscover,
  deployHistory,
  deployList,
  deployLogs,
  deployPromote,
  deployRefresh,
  deployRetry,
  deployRollback,
  deploySetScope,
  deployStatus,
  deployTargetAdd,
  deployTargetRemove,
  deployTargetUpdate,
  deployTargets,
  deployTrigger,
} from "../lib/api";
import type {
  DeployCandidate,
  DeployHistoryItem,
  DeployProvider,
  DeployProviderStatus,
  DeployState,
  DeployTarget,
  Deployment,
} from "../lib/types";
import { fmtAgo, fmtUptime } from "../lib/format";
import { Badge, Button, Card, Empty, Modal, inputClass } from "../components/ui";

type Tone = "slate" | "sky" | "amber" | "emerald" | "rose" | "violet";
type Platform = "vercel" | "cloudflare";

const PLATFORMS: Record<Platform, { label: string; hint: string; url: string }> = {
  vercel: {
    label: "Vercel",
    hint: "在 Account Settings → Tokens 里创建，范围选「团队」或「整个账户」；只限单个项目的令牌看不到别的项目",
    url: "https://vercel.com/account/settings/tokens",
  },
  cloudflare: {
    label: "Cloudflare",
    hint: "在 My Profile → API Tokens 里创建：Pages 要「Cloudflare Pages: Edit」；还要管 Worker 的构建，再加「Workers Builds Configuration: Edit」和「Workers Scripts: Read」，并且要建成用户级令牌（账户级令牌会被构建接口拒绝）",
    url: "https://dash.cloudflare.com/profile/api-tokens",
  },
};

const PROVIDER: Record<DeployProvider, { label: string; tone: Tone }> = {
  vercel: { label: "Vercel", tone: "sky" },
  "cloudflare-pages": { label: "Pages", tone: "amber" },
  "cloudflare-workers": { label: "Workers", tone: "violet" },
};

/** 部署状态的文案与颜色（任务页也用） */
export const DEPLOY_STATE: Record<DeployState, { label: string; tone: Tone }> = {
  queued: { label: "排队中", tone: "slate" },
  building: { label: "构建中", tone: "sky" },
  ready: { label: "成功", tone: "emerald" },
  error: { label: "失败", tone: "rose" },
  canceled: { label: "已取消", tone: "slate" },
  other: { label: "已结束", tone: "slate" },
};

const DOT: Record<Tone, string> = {
  slate: "bg-slate-400",
  sky: "bg-sky-400",
  amber: "bg-amber-400",
  emerald: "bg-emerald-400",
  rose: "bg-rose-400",
  violet: "bg-violet-400",
};

const selectClass =
  "rounded-lg border border-slate-700 bg-slate-950 px-2 py-1 text-xs text-slate-300 focus:border-sky-600 focus:outline-none";

const sha7 = (sha: string | null) => (sha ? sha.slice(0, 7) : "—");

/** 从提交到就绪用了多久 */
function fmtDuration(dep: Deployment): string {
  if (dep.buildingMs && dep.readyMs && dep.readyMs >= dep.buildingMs) {
    return fmtUptime((dep.readyMs - dep.buildingMs) / 1000);
  }
  return "—";
}

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** 确认一件有后果的事 */
function ConfirmDialog({
  title,
  children,
  confirm = "确认",
  danger = false,
  onConfirm,
  onClose,
}: {
  title: string;
  children: ReactNode;
  confirm?: string;
  danger?: boolean;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Modal
      title={title}
      onClose={onClose}
      width="max-w-md"
      footer={
        <>
          <Button onClick={onClose}>算了</Button>
          <Button variant={danger ? "danger" : "primary"} onClick={onConfirm}>
            {confirm}
          </Button>
        </>
      }
    >
      <div className="text-sm leading-6 text-slate-300">{children}</div>
    </Modal>
  );
}

/** 一家平台的连接状态：令牌、团队 / 账户 */
function ProviderCard({
  platform,
  status,
  onToken,
}: {
  platform: Platform;
  status: DeployProviderStatus;
  onToken: () => void;
}) {
  const qc = useQueryClient();
  const [error, setError] = useState<string | null>(null);
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ["deployStatus"] });
    qc.invalidateQueries({ queryKey: ["deployTargets"] });
  };
  const pick = useMutation({
    mutationFn: (scope: string) => deploySetScope(platform, scope),
    onSuccess: () => {
      setError(null);
      refresh();
    },
    onError: (e) => setError(errText(e)),
  });
  const drop = useMutation({ mutationFn: () => deployDisconnect(platform), onSuccess: refresh });
  const ok = status.configured && !status.error;
  return (
    <div className="rounded-lg border border-slate-800 bg-slate-900/60 p-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-medium text-white">{PLATFORMS[platform].label}</span>
        {status.error ? (
          <Badge tone="rose">令牌无效</Badge>
        ) : ok ? (
          <Badge tone="emerald">已连接{status.who ? ` · ${status.who}` : ""}</Badge>
        ) : (
          <Badge tone="slate">未连接</Badge>
        )}
      </div>
      {status.error && <p className="mt-2 text-xs leading-5 text-rose-400">{status.error}</p>}
      {ok && status.scopes.length > 0 && (
        <select
          aria-label={`${PLATFORMS[platform].label} 的团队或账户`}
          className={`mt-2 w-full ${selectClass}`}
          value={status.scope ?? ""}
          onChange={(e) => pick.mutate(e.target.value)}
        >
          {status.scope && !status.scopes.some((s) => s.id === status.scope) && (
            <option value={status.scope}>{status.scope}</option>
          )}
          {status.scopes.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </select>
      )}
      {error && <p className="mt-2 text-xs leading-5 text-rose-400">{error}</p>}
      <div className="mt-2 flex gap-3 text-xs">
        <button className="text-sky-400 hover:text-sky-300" onClick={onToken}>
          {status.configured ? "更换令牌" : "填入令牌"}
        </button>
        {status.configured && (
          <button className="text-slate-500 hover:text-slate-300" onClick={() => drop.mutate()}>
            断开
          </button>
        )}
      </div>
    </div>
  );
}

/** 填入 API 令牌 */
function TokenDialog({ platform, onClose }: { platform: Platform; onClose: () => void }) {
  const qc = useQueryClient();
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const connect = useMutation({
    mutationFn: () => deployConnect(platform, token),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ["deployStatus"] });
      onClose();
    },
    onError: (e) => setError(errText(e)),
  });
  return (
    <Modal
      title={`连接 ${PLATFORMS[platform].label}`}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="primary"
            disabled={!token.trim() || connect.isPending}
            onClick={() => connect.mutate()}
          >
            {connect.isPending ? "验证中…" : "验证并保存"}
          </Button>
        </>
      }
    >
      <input
        type="password"
        value={token}
        onChange={(e) => {
          setToken(e.target.value);
          setError(null);
        }}
        placeholder="粘贴 API 令牌"
        aria-label="API 令牌"
        className={inputClass}
      />
      <p className="mt-3 text-xs leading-5 text-slate-500">
        {PLATFORMS[platform].hint}。{" "}
        <button className="text-sky-400 hover:text-sky-300" onClick={() => openUrl(PLATFORMS[platform].url)}>
          去创建 ↗
        </button>
      </p>
      <p className="mt-2 text-xs text-slate-600">令牌只存进系统钥匙串：不进数据库，之后也不会显示出来。</p>
      {error && (
        <p role="alert" className="mt-3 text-xs leading-5 text-rose-400">
          {error}
        </p>
      )}
    </Modal>
  );
}

/** 把平台上的项目登记成部署目标 */
function AddTargetDialog({ onClose, onAdded }: { onClose: () => void; onAdded: (id: number) => void }) {
  const qc = useQueryClient();
  const status = useQuery({ queryKey: ["deployStatus"], queryFn: deployStatus });
  const [platform, setPlatform] = useState<Platform | null>(null);
  const [chosen, setChosen] = useState<string[]>([]);
  /** Workers 的生产地址与 Deploy Hook：登记时可以一并给 */
  const [extra, setExtra] = useState<Record<string, { url: string; hook: string }>>({});
  const [error, setError] = useState<string | null>(null);

  // 打开时默认停在第一个已连接的平台
  useEffect(() => {
    if (platform || !status.data) return;
    const first = (["vercel", "cloudflare"] as const).find((p) => {
      const s = status.data?.[p];
      return s.configured && !s.error;
    });
    setPlatform(first ?? "vercel");
  }, [status.data, platform]);

  const connected = (p: Platform) => {
    const s = status.data?.[p];
    return !!s && s.configured && !s.error;
  };
  const found = useQuery({
    queryKey: ["deployDiscover", platform],
    queryFn: () => deployDiscover(platform ?? "vercel"),
    enabled: platform !== null && connected(platform ?? "vercel"),
  });

  const toggle = (c: DeployCandidate) =>
    setChosen((list) =>
      list.includes(c.platformId) ? list.filter((id) => id !== c.platformId) : [...list, c.platformId],
    );
  const patch = (id: string, fields: Partial<{ url: string; hook: string }>) =>
    setExtra((all) => {
      const prev = all[id] ?? { url: "", hook: "" };
      return { ...all, [id]: { ...prev, ...fields } };
    });

  const add = useMutation({
    mutationFn: async () => {
      const picks = (found.data ?? []).filter((c) => chosen.includes(c.platformId));
      let last: DeployTarget | null = null;
      for (const c of picks) {
        const x = extra[c.platformId];
        last = await deployTargetAdd(c.provider, c.name, c.platformId, x?.url || null, x?.hook || null);
      }
      return last;
    },
    onSuccess: (last) => {
      qc.invalidateQueries({ queryKey: ["deployTargets"] });
      qc.invalidateQueries({ queryKey: ["deployments"] });
      if (last) onAdded(last.id);
      onClose();
    },
    onError: (e) => setError(errText(e)),
  });

  return (
    <Modal
      title="登记项目"
      onClose={onClose}
      width="max-w-xl"
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button variant="primary" disabled={chosen.length === 0 || add.isPending} onClick={() => add.mutate()}>
            {add.isPending ? "登记中…" : chosen.length ? `登记 ${chosen.length} 个` : "登记"}
          </Button>
        </>
      }
    >
      <div className="flex gap-1">
        {(["vercel", "cloudflare"] as const).map((p) => (
          <button
            key={p}
            disabled={!connected(p)}
            onClick={() => {
              setPlatform(p);
              setChosen([]);
              setError(null);
            }}
            className={`rounded-md border px-2.5 py-1 text-xs transition disabled:cursor-not-allowed disabled:opacity-40 ${
              platform === p
                ? "border-sky-600 bg-sky-950/50 text-white"
                : "border-slate-700 text-slate-300 hover:border-sky-600"
            }`}
          >
            {PLATFORMS[p].label}
            {!connected(p) && "（未连接）"}
          </button>
        ))}
      </div>
      <div className="mt-3 max-h-96 space-y-1 overflow-y-auto">
        {found.isLoading && <p className="py-6 text-center text-xs text-slate-500">正在向平台取项目清单…</p>}
        {found.isError && (
          <p className="rounded-lg border border-rose-900/60 bg-rose-950/30 px-3 py-2 text-xs leading-5 text-rose-300">
            {errText(found.error)}
          </p>
        )}
        {found.data?.map((c) => {
          const on = chosen.includes(c.platformId);
          return (
            <div key={`${c.provider}/${c.platformId}`} className="rounded-lg border border-slate-800 px-3 py-2">
              <label className="flex cursor-pointer items-center gap-3">
                <input
                  type="checkbox"
                  aria-label={c.name}
                  disabled={c.registered != null}
                  checked={on || c.registered != null}
                  onChange={() => toggle(c)}
                />
                <span className="min-w-0 flex-1">
                  <span className="text-sm text-slate-200">{c.name}</span>
                  <span className="ml-2">
                    <Badge tone={PROVIDER[c.provider].tone}>{PROVIDER[c.provider].label}</Badge>
                  </span>
                  <span className="ml-2 text-xs text-slate-500">{[c.repo, c.branch].filter(Boolean).join(" · ")}</span>
                </span>
                {c.registered != null && <span className="text-xs text-slate-600">已登记</span>}
              </label>
              {on && c.provider === "cloudflare-workers" && (
                <div className="mt-2 grid grid-cols-1 gap-2 pl-6 sm:grid-cols-2">
                  <input
                    className={inputClass}
                    placeholder="生产地址（可选，如 https://api.example.com）"
                    aria-label={`${c.name} 的生产地址`}
                    value={extra[c.platformId]?.url ?? ""}
                    onChange={(e) => patch(c.platformId, { url: e.target.value })}
                  />
                  <input
                    className={inputClass}
                    placeholder="Deploy Hook 地址（可选，主动触发构建用）"
                    aria-label={`${c.name} 的 Deploy Hook`}
                    value={extra[c.platformId]?.hook ?? ""}
                    onChange={(e) => patch(c.platformId, { hook: e.target.value })}
                  />
                </div>
              )}
            </div>
          );
        })}
        {found.isSuccess && found.data.length === 0 && (
          <p className="py-6 text-center text-xs text-slate-500">这个账户下还没有项目</p>
        )}
      </div>
      <p className="mt-3 text-xs leading-5 text-slate-600">
        登记后每分钟从平台拉一次部署状态；项目关联了 GitHub 仓库，出问题时才能自动建修复任务。
      </p>
      {error && (
        <p role="alert" className="mt-2 text-xs leading-5 text-rose-400">
          {error}
        </p>
      )}
    </Modal>
  );
}

/** 主动部署 */
function DeployDialog({ target, onClose, onDone }: { target: DeployTarget; onClose: () => void; onDone: () => void }) {
  const [branch, setBranch] = useState(target.branch ?? "");
  const [commit, setCommit] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const workers = target.provider === "cloudflare-workers";
  const production = !workers && !!target.branch && branch.trim() === target.branch;
  const needsConfirm = target.prodRequiresApproval && (workers || production);

  const run = useMutation({
    mutationFn: () =>
      deployTrigger(target.id, workers ? null : branch.trim() || null, commit.trim() || null, confirmed),
    onSuccess: onDone,
    onError: (e) => setError(errText(e)),
  });

  return (
    <Modal
      title={`部署 ${target.name}`}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="primary"
            disabled={
              run.isPending ||
              (workers && !target.hasHook) ||
              (needsConfirm && !confirmed) ||
              (!workers && !branch.trim())
            }
            onClick={() => run.mutate()}
          >
            {run.isPending ? "发起中…" : production || workers ? "部署到生产" : "部署预览"}
          </Button>
        </>
      }
    >
      {workers ? (
        <p className="text-sm leading-6 text-slate-300">
          触发这个 Worker 的 Deploy Hook，让它现在构建一次。构建哪条分支由 Hook 决定，多半就是生产分支。
          {!target.hasHook && (
            <span className="mt-2 block text-amber-400">
              还没有配 Deploy Hook：在 Cloudflare 控制台「Settings → Builds → Deploy Hooks」里建一个，填到这个项目的设置里。
            </span>
          )}
        </p>
      ) : (
        <>
          <label className="block text-xs text-slate-500">
            分支
            <input
              className={`mt-1 font-mono ${inputClass}`}
              value={branch}
              onChange={(e) => {
                setBranch(e.target.value);
                setConfirmed(false);
              }}
              placeholder={target.branch ?? "main"}
            />
          </label>
          <label className="mt-3 block text-xs text-slate-500">
            提交（可选，留空用分支上最新的）
            <input
              className={`mt-1 font-mono ${inputClass}`}
              value={commit}
              onChange={(e) => setCommit(e.target.value)}
              placeholder="完整或前几位提交号"
            />
          </label>
          <p className="mt-3 text-xs leading-5 text-slate-500">
            {target.provider === "cloudflare-pages"
              ? "Cloudflare Pages 没有指定分支发起部署的接口：这里是把这条分支最近的一次原样重做一遍。这条分支还没部署过的话，推送后会自动部署。"
              : "按这条分支上最新的提交发一次新的部署。"}
            {production ? " 这是生产分支：部署成功后生产域名会指向它。" : " 不是生产分支：生成的是预览部署。"}
          </p>
        </>
      )}
      {needsConfirm && (
        <label className="mt-4 flex cursor-pointer items-start gap-2 text-sm text-amber-300">
          <input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} />
          我确认要部署到生产环境
        </label>
      )}
      {error && (
        <p role="alert" className="mt-3 text-xs leading-5 text-rose-400">
          {error}
        </p>
      )}
    </Modal>
  );
}

/** 回滚到之前的某一次 */
function RollbackDialog({ target, onClose, onDone }: { target: DeployTarget; onClose: () => void; onDone: () => void }) {
  const [choice, setChoice] = useState<DeployHistoryItem | null>(null);
  const [confirmed, setConfirmed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const history = useQuery({ queryKey: ["deployHistory", target.id], queryFn: () => deployHistory(target.id) });
  const run = useMutation({
    mutationFn: () => deployRollback(target.id, choice!.id, choice!.versionId),
    onSuccess: onDone,
    onError: (e) => setError(errText(e)),
  });
  return (
    <Modal
      title={`回滚 ${target.name}`}
      onClose={onClose}
      width="max-w-lg"
      footer={
        <>
          <Button onClick={onClose}>取消</Button>
          <Button
            variant="danger"
            disabled={!choice || choice.current || !confirmed || run.isPending}
            onClick={() => run.mutate()}
          >
            {run.isPending ? "回滚中…" : "回滚到这一次"}
          </Button>
        </>
      }
    >
      <div className="max-h-72 space-y-1 overflow-y-auto">
        {history.isLoading && <p className="py-4 text-center text-xs text-slate-500">正在取历次部署…</p>}
        {history.isError && <p className="text-xs text-rose-400">{errText(history.error)}</p>}
        {history.data?.map((item) => (
          <label
            key={item.id}
            className={`flex cursor-pointer items-center gap-3 rounded-lg border px-3 py-2 ${
              choice?.id === item.id ? "border-sky-600 bg-sky-950/40" : "border-slate-800 hover:border-slate-600"
            }`}
          >
            <input
              type="radio"
              name="rollback"
              disabled={item.current}
              checked={choice?.id === item.id}
              onChange={() => {
                setChoice(item);
                setConfirmed(false);
              }}
            />
            <span className="min-w-0 flex-1 truncate text-xs text-slate-200">{item.commitMessage ?? item.id}</span>
            {item.branch && <span className="font-mono text-[11px] text-slate-500">{item.branch}</span>}
            <span className="shrink-0 text-[11px] text-slate-500">{fmtAgo(item.createdMs)}</span>
            {item.current && <Badge tone="emerald">当前</Badge>}
          </label>
        ))}
        {history.isSuccess && history.data.length === 0 && (
          <p className="py-4 text-center text-xs text-slate-500">还没有成功的生产部署可以回去</p>
        )}
      </div>
      <p className="mt-3 text-xs leading-5 text-amber-400/90">
        {target.provider === "vercel"
          ? "回滚后 Vercel 会关闭生产域名的自动分配：之后推送到生产分支不会再自动上线，要手动把某次部署提升为生产。个人版只能回到上一次。"
          : target.provider === "cloudflare-pages"
            ? "把生产环境切回这一次部署。"
            : "把选中的版本按 100% 的流量重新部署；它之后对 KV、D1 这些资源的改动不会跟着回退。"}
      </p>
      {choice && !choice.current && (
        <label className="mt-3 flex cursor-pointer items-start gap-2 text-sm text-amber-300">
          <input type="checkbox" className="mt-1" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} />
          我确认把生产环境切回去
        </label>
      )}
      {error && (
        <p role="alert" className="mt-3 text-xs leading-5 text-rose-400">
          {error}
        </p>
      )}
    </Modal>
  );
}

/** 一次部署的构建日志 */
function LogPanel({ dep, onClose }: { dep: Deployment; onClose: () => void }) {
  const logs = useQuery({ queryKey: ["deployLogs", dep.id], queryFn: () => deployLogs(dep.id) });
  return (
    <Card className="mt-4 overflow-hidden">
      <div className="flex items-center justify-between gap-3 border-b border-slate-800/80 px-4 py-2">
        <div className="flex min-w-0 items-center gap-2 text-xs text-slate-400">
          <span>构建日志</span>
          <Badge tone={DEPLOY_STATE[dep.state].tone}>{DEPLOY_STATE[dep.state].label}</Badge>
          <span className="font-mono text-sky-400">{sha7(dep.commitSha)}</span>
          <span className="truncate">{dep.commitMessage ?? ""}</span>
          {dep.errorMessage && <span className="truncate text-rose-400">{dep.errorMessage}</span>}
        </div>
        <div className="flex shrink-0 items-center gap-3 text-xs">
          {dep.inspectorUrl && (
            <button className="text-sky-400 hover:text-sky-300" onClick={() => openUrl(dep.inspectorUrl!)}>
              控制台 ↗
            </button>
          )}
          <button className="text-slate-500 hover:text-slate-300" onClick={onClose}>
            收起
          </button>
        </div>
      </div>
      {logs.isError ? (
        <p className="px-4 py-3 text-xs text-rose-400">{errText(logs.error)}</p>
      ) : (
        <pre className="max-h-80 overflow-auto px-4 py-3 font-mono text-[11px] leading-5 text-slate-300">
          {logs.data ?? "正在取日志…"}
        </pre>
      )}
    </Card>
  );
}

/** 选中项目的详情：设置、部署列表、日志 */
function TargetDetail({ id, onRemoved }: { id: number; onRemoved: () => void }) {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const targets = useQuery({ queryKey: ["deployTargets"], queryFn: deployTargets });
  const target = targets.data?.find((t) => t.id === id) ?? null;
  const deployments = useQuery({
    queryKey: ["deployments", id],
    queryFn: () => deployList(id, 50),
    refetchInterval: 5000,
  });
  const [logFor, setLogFor] = useState<Deployment | null>(null);
  const [deploying, setDeploying] = useState(false);
  const [rolling, setRolling] = useState(false);
  const [confirm, setConfirm] = useState<{ text: ReactNode; action: () => Promise<unknown>; danger?: boolean; label: string } | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);

  /** 平台上的状态变了：拉一次，再刷新本地记录 */
  const sync = async () => {
    try {
      await deployRefresh(id);
    } catch (e) {
      setNotice({ kind: "error", text: errText(e) });
    }
    await qc.invalidateQueries({ queryKey: ["deployments"] });
    await qc.invalidateQueries({ queryKey: ["deployTargets"] });
  };
  /** 做一件会改动线上状态的事；出错时把平台给的原因留在页面上 */
  const act = (what: string, run: () => Promise<unknown>) => async () => {
    setNotice(null);
    try {
      await run();
      await sync();
      setNotice({ kind: "ok", text: `${what}：已发给平台，稍后状态会更新` });
    } catch (e) {
      setNotice({ kind: "error", text: `${what}失败：${errText(e)}` });
    }
  };
  /** 手动刷新：拉一次平台，把结果告诉用户 */
  const manualRefresh = async () => {
    setNotice(null);
    try {
      const changed = await deployRefresh(id);
      setNotice({ kind: "ok", text: changed ? `已刷新：${changed} 条部署的状态变了` : "已刷新：没有变化" });
    } catch (e) {
      setNotice({ kind: "error", text: `刷新失败：${errText(e)}` });
    }
    await qc.invalidateQueries({ queryKey: ["deployments"] });
    await qc.invalidateQueries({ queryKey: ["deployTargets"] });
  };
  const dialogDone = (what: string, close: () => void) => () => {
    close();
    void act(what, async () => {})();
  };

  // 设置表单：跟着目标走
  const [form, setForm] = useState({
    url: "",
    prodRequiresApproval: true,
    autoFix: true,
    healthPath: "/",
    healthContains: "",
    hookUrl: "",
  });
  useEffect(() => {
    if (!target) return;
    setForm({
      url: target.url ?? "",
      prodRequiresApproval: target.prodRequiresApproval,
      autoFix: target.autoFix,
      healthPath: target.healthPath,
      healthContains: target.healthContains ?? "",
      hookUrl: "",
    });
  }, [target?.id, target?.url, target?.prodRequiresApproval, target?.autoFix, target?.healthPath, target?.healthContains]);
  const dirty =
    !!target &&
    (form.url !== (target.url ?? "") ||
      form.prodRequiresApproval !== target.prodRequiresApproval ||
      form.autoFix !== target.autoFix ||
      form.healthPath !== target.healthPath ||
      form.healthContains !== (target.healthContains ?? "") ||
      form.hookUrl.trim() !== "");
  const save = useMutation({
    mutationFn: () =>
      deployTargetUpdate(id, {
        url: form.url.trim() || null,
        prodRequiresApproval: form.prodRequiresApproval,
        autoFix: form.autoFix,
        healthPath: form.healthPath.trim() || null,
        healthContains: form.healthContains,
        hookUrl: form.hookUrl.trim() || undefined,
      }),
    onSuccess: async () => {
      setNotice({ kind: "ok", text: "设置已保存" });
      setForm((f) => ({ ...f, hookUrl: "" }));
      await qc.invalidateQueries({ queryKey: ["deployTargets"] });
    },
    onError: (e) => setNotice({ kind: "error", text: `保存失败：${errText(e)}` }),
  });

  if (!target) {
    if (targets.isSuccess) {
      return (
        <div className="p-8">
          <Empty title="这个项目已经取消登记了" desc="在左边选另一个，或者重新登记" />
        </div>
      );
    }
    return null;
  }
  const current = deployments.data?.find((d) => d.current);
  const P = PROVIDER[target.provider];

  return (
    <div className="p-6">
      {/* 概要 */}
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h2 className="text-lg font-semibold text-white">{target.name}</h2>
            <Badge tone={P.tone}>{P.label}</Badge>
            {current && <Badge tone="emerald">当前生产</Badge>}
          </div>
          <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1 text-xs text-slate-500">
            {target.repo && <span>仓库 {target.repo}</span>}
            {target.branch && <span className="font-mono">生产分支 {target.branch}</span>}
            {target.framework && <span>{target.framework}</span>}
            {target.url && (
              <button className="text-sky-400 hover:text-sky-300" onClick={() => openUrl(target.url!)}>
                {target.url.replace(/^https?:\/\//, "")} ↗
              </button>
            )}
          </div>
          {current && (
            <p className="mt-2 text-xs text-slate-400">
              线上是 <span className="font-mono text-sky-400">{sha7(current.commitSha)}</span>{" "}
              {current.commitMessage ?? ""} · {fmtAgo(current.readyMs ?? current.createdMs)}
              {current.health && (
                <>
                  {" "}
                  · 健康{" "}
                  {current.health === "ok" ? (
                    <span className="text-emerald-400">正常</span>
                  ) : (
                    <span className="text-rose-400">{current.health}</span>
                  )}
                </>
              )}
            </p>
          )}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button onClick={() => void manualRefresh()}>刷新</Button>
          <Button variant="primary" onClick={() => setDeploying(true)}>
            部署…
          </Button>
          <Button onClick={() => setRolling(true)}>回滚…</Button>
          <Button
            variant="danger"
            onClick={() =>
              setConfirm({
                text: <>取消登记「{target.name}」：本地的部署记录会删掉，平台上的项目和线上服务都不动。</>,
                label: "取消登记",
                danger: true,
                action: () =>
                  deployTargetRemove(id).then(() => {
                    qc.invalidateQueries({ queryKey: ["deployTargets"] });
                    qc.invalidateQueries({ queryKey: ["deployments"] });
                    onRemoved();
                  }),
              })
            }
          >
            取消登记
          </Button>
        </div>
      </div>

      {notice && (
        <div
          role="status"
          className={`mt-4 rounded-lg border px-3 py-2 text-xs leading-5 ${
            notice.kind === "ok"
              ? "border-sky-900 bg-sky-950/40 text-sky-200"
              : "border-rose-900/70 bg-rose-950/30 text-rose-200"
          }`}
        >
          {notice.text}
        </div>
      )}

      {/* 守护设置 */}
      <Card className="mt-5 p-4">
        <div className="text-sm font-medium text-slate-200">守护</div>
        <p className="mt-1 text-xs leading-5 text-slate-500">
          每分钟从平台拉一次部署。生产部署成功后访问「生产地址 + 检查路径」，返回 2xx（设了期望内容的话还要包含它）才算正常；
          构建失败或检查不过时取构建日志，自动建一个修复任务，沿用上次处理这个仓库的机器和 Agent 开工，改完仍要你确认才发 PR。
          同一个项目 24 小时内最多自动建三次。
        </p>
        <div className="mt-3 grid grid-cols-1 gap-x-6 gap-y-3 lg:grid-cols-2">
          <label className="flex items-center gap-2 text-sm text-slate-300">
            <input
              type="checkbox"
              checked={form.prodRequiresApproval}
              onChange={(e) => setForm({ ...form, prodRequiresApproval: e.target.checked })}
            />
            在这里发起生产部署要先确认
          </label>
          <label className="flex items-center gap-2 text-sm text-slate-300">
            <input
              type="checkbox"
              checked={form.autoFix}
              onChange={(e) => setForm({ ...form, autoFix: e.target.checked })}
            />
            生产出问题时自动建修复任务
          </label>
          <label className="block text-xs text-slate-500">
            生产地址
            <input
              className={`mt-1 ${inputClass}`}
              value={form.url}
              onChange={(e) => setForm({ ...form, url: e.target.value })}
              placeholder="https://…"
            />
          </label>
          <div className="grid grid-cols-5 gap-2">
            <label className="col-span-3 block text-xs text-slate-500">
              健康检查路径
              <input
                className={`mt-1 font-mono ${inputClass}`}
                value={form.healthPath}
                onChange={(e) => setForm({ ...form, healthPath: e.target.value })}
                placeholder="/"
              />
            </label>
            <label className="col-span-2 block text-xs text-slate-500">
              期望包含
              <input
                className={`mt-1 ${inputClass}`}
                value={form.healthContains}
                onChange={(e) => setForm({ ...form, healthContains: e.target.value })}
                placeholder="可留空"
              />
            </label>
          </div>
          {target.provider === "cloudflare-workers" && (
            <label className="block text-xs text-slate-500 lg:col-span-2">
              Deploy Hook 地址{target.hasHook ? "（已保存；留空不改）" : "（还没配：没有它就不能在这里主动触发构建）"}
              <input
                type="password"
                className={`mt-1 ${inputClass}`}
                value={form.hookUrl}
                onChange={(e) => setForm({ ...form, hookUrl: e.target.value })}
                placeholder="https://api.cloudflare.com/client/v4/workers/builds/deploy_hooks/…"
              />
            </label>
          )}
        </div>
        <div className="mt-3 flex items-center gap-3">
          <Button variant="primary" disabled={!dirty || save.isPending} onClick={() => save.mutate()}>
            {save.isPending ? "保存中…" : "保存设置"}
          </Button>
          {target.provider === "vercel" && (
            <span className="text-[11px] text-slate-600">
              Vercel 的生产地址是从最近一次成功的生产部署上认的，不对可以在这里改。
            </span>
          )}
        </div>
      </Card>

      {/* 部署列表 */}
      <div className="mt-5 flex items-center justify-between">
        <div className="text-sm font-medium text-slate-200">部署记录</div>
        <span className="text-xs text-slate-500">点一行看构建日志</span>
      </div>
      <Card className="mt-2 overflow-x-auto">
        <table className="w-full text-left text-xs">
          <thead>
            <tr className="border-b border-slate-800/80 text-slate-500">
              <th className="px-3 py-2 font-normal">状态</th>
              <th className="px-3 py-2 font-normal">环境</th>
              <th className="px-3 py-2 font-normal">提交</th>
              <th className="px-3 py-2 font-normal">分支</th>
              <th className="px-3 py-2 font-normal">耗时</th>
              <th className="px-3 py-2 font-normal">时间</th>
              <th className="px-3 py-2 font-normal">健康</th>
              <th className="px-3 py-2 font-normal">任务</th>
              <th className="px-3 py-2 text-right font-normal">操作</th>
            </tr>
          </thead>
          <tbody>
            {deployments.data?.map((d) => {
              const live = d.state === "queued" || d.state === "building";
              const isProduction = d.env === "production";
              const retry = () => deployRetry(d.id, isProduction && target.prodRequiresApproval);
              return (
                <tr
                  key={d.id}
                  onClick={() => setLogFor(d)}
                  className={`cursor-pointer border-b border-slate-800/50 transition last:border-0 hover:bg-slate-800/40 ${
                    logFor?.id === d.id ? "bg-slate-800/60" : ""
                  }`}
                >
                  <td className="px-3 py-2">
                    <span className="inline-flex items-center gap-1.5">
                      {live && <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-sky-400" />}
                      <Badge tone={DEPLOY_STATE[d.state].tone}>{DEPLOY_STATE[d.state].label}</Badge>
                      {d.current && <Badge tone="emerald">当前</Badge>}
                    </span>
                  </td>
                  <td className="px-3 py-2 text-slate-400">{isProduction ? "生产" : "预览"}</td>
                  <td className="max-w-64 px-3 py-2">
                    <span className="font-mono text-[11px] text-sky-400">{sha7(d.commitSha)}</span>{" "}
                    <span className="text-slate-300" title={d.errorMessage ?? d.commitMessage ?? ""}>
                      {d.state === "error" && d.errorMessage ? d.errorMessage : (d.commitMessage ?? "—")}
                    </span>
                  </td>
                  <td className="px-3 py-2 font-mono text-[11px] text-slate-500">{d.branch ?? "—"}</td>
                  <td className="px-3 py-2 text-slate-400">{fmtDuration(d)}</td>
                  <td className="px-3 py-2 text-slate-500">{fmtAgo(d.createdMs)}</td>
                  <td className="px-3 py-2">
                    {d.state === "ready" && isProduction ? (
                      <span className="inline-flex items-center gap-1.5">
                        {d.health == null ? (
                          <span className="text-slate-500">待检查</span>
                        ) : d.health === "ok" ? (
                          <Badge tone="emerald">正常</Badge>
                        ) : (
                          <Badge tone={d.health.includes("连不上") || d.health.includes("访问保护") ? "slate" : "rose"}>
                            {d.health}
                          </Badge>
                        )}
                        <button
                          className="text-slate-500 hover:text-sky-300"
                          onClick={(e) => {
                            e.stopPropagation();
                            void act(
                              "健康检查",
                              async () => {
                                const text = await deployCheckHealth(d.id);
                                if (text !== "ok") throw new Error(text);
                              },
                            )();
                          }}
                        >
                          复查
                        </button>
                      </span>
                    ) : (
                      <span className="text-slate-600">—</span>
                    )}
                  </td>
                  <td className="px-3 py-2">
                    {d.taskId != null && (
                      <button
                        className="text-xs text-violet-400 hover:text-violet-300"
                        onClick={(e) => {
                          e.stopPropagation();
                          navigate(`/tasks?t=${d.taskId}`);
                        }}
                      >
                        修复任务 #{d.taskId} →
                      </button>
                    )}
                  </td>
                  <td className="px-3 py-2" onClick={(e) => e.stopPropagation()}>
                    <span className="inline-flex flex-wrap justify-end gap-x-3 gap-y-1 text-xs">
                      {d.url && (
                        <button className="text-sky-400 hover:text-sky-300" onClick={() => openUrl(d.url!)}>
                          打开
                        </button>
                      )}
                      <button className="text-slate-400 hover:text-white" onClick={() => setLogFor(d)}>
                        日志
                      </button>
                      {live && target.provider !== "cloudflare-pages" && (
                        <button
                          className="text-slate-400 hover:text-rose-300"
                          onClick={() =>
                            setConfirm({
                              text: "取消这次还在进行的部署？平台会停掉正在跑的构建。",
                              label: "取消部署",
                              danger: true,
                              action: () => deployCancel(d.id),
                            })
                          }
                        >
                          取消
                        </button>
                      )}
                      {!live && d.state !== "other" && (
                        <button
                          className="text-slate-400 hover:text-white"
                          onClick={() =>
                            isProduction && target.prodRequiresApproval
                              ? setConfirm({
                                  text: (
                                    <>
                                      把 <span className="font-mono text-sky-400">{sha7(d.commitSha)}</span>{" "}
                                      这次生产部署原样重做一遍？用分支上最新的提交，会直接上线。
                                    </>
                                  ),
                                  label: "确认重做",
                                  action: retry,
                                })
                              : void act("重新部署", retry)()
                          }
                        >
                          重做
                        </button>
                      )}
                      {target.provider === "vercel" && d.state === "ready" && !isProduction && (
                        <button
                          className="text-slate-400 hover:text-amber-300"
                          onClick={() =>
                            setConfirm({
                              text: "把这次预览部署直接提升为生产？不会重新构建，用的是预览环境的那份产物。",
                              label: "提升为生产",
                              action: () => deployPromote(d.id),
                            })
                          }
                        >
                          升为生产
                        </button>
                      )}
                      {isProduction && d.state === "ready" && !d.current && (d.rollbackOk || target.provider !== "vercel") && (
                        <button className="text-slate-400 hover:text-amber-300" onClick={() => setRolling(true)}>
                          回滚到这一次
                        </button>
                      )}
                    </span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
        {deployments.isSuccess && deployments.data.length === 0 && (
          <p className="p-6 text-center text-xs text-slate-500">
            还没有部署记录。这个项目在平台上还没部署过的话，推送一次代码就会出现在这里。
          </p>
        )}
      </Card>

      {logFor && <LogPanel dep={logFor} onClose={() => setLogFor(null)} />}

      {deploying && (
        <DeployDialog
          target={target}
          onClose={() => setDeploying(false)}
          onDone={dialogDone("部署", () => setDeploying(false))}
        />
      )}
      {rolling && (
        <RollbackDialog
          target={target}
          onClose={() => setRolling(false)}
          onDone={dialogDone("回滚", () => setRolling(false))}
        />
      )}
      {confirm && (
        <ConfirmDialog
          title="请确认"
          confirm={confirm.label}
          danger={confirm.danger}
          onClose={() => setConfirm(null)}
          onConfirm={() => {
            const { action, label } = confirm;
            setConfirm(null);
            void act(label, action)();
          }}
        >
          {confirm.text}
        </ConfirmDialog>
      )}
    </div>
  );
}

export default function Deploy() {
  const [params, setParams] = useSearchParams();
  const [tokenFor, setTokenFor] = useState<Platform | null>(null);
  const [adding, setAdding] = useState(false);
  const selected = Number(params.get("t")) || null;

  const status = useQuery({ queryKey: ["deployStatus"], queryFn: deployStatus });
  const targets = useQuery({ queryKey: ["deployTargets"], queryFn: deployTargets });
  const all = useQuery({ queryKey: ["deployments"], queryFn: () => deployList(null), refetchInterval: 5000 });

  const pick = (id: number) => setParams({ t: String(id) }, { replace: true });
  const clear = () => setParams({}, { replace: true });
  const connected = (p: Platform) => {
    const s = status.data?.[p];
    return !!s && s.configured && !s.error;
  };
  const anyConnected = connected("vercel") || connected("cloudflare");
  // 清单里每个项目最近一次部署的状态，用来在左边标出来
  const latest = new Map<number, Deployment>();
  for (const d of all.data ?? []) if (!latest.has(d.targetId)) latest.set(d.targetId, d);

  return (
    <div className="flex h-full">
      <aside className="flex w-80 shrink-0 flex-col border-r border-slate-800/80">
        <div className="p-4 pb-3">
          <h1 className="text-lg font-semibold text-white">部署</h1>
          <p className="mt-0.5 text-xs leading-5 text-slate-500">
            连接 Vercel / Cloudflare，登记项目；之后自动跟踪每次部署，生产出问题时建修复任务
          </p>
        </div>
        <div className="space-y-2 px-3">
          {(["vercel", "cloudflare"] as const).map((p) => (
            <ProviderCard
              key={p}
              platform={p}
              status={status.data?.[p] ?? { configured: false, error: null, who: null, scopes: [], scope: null }}
              onToken={() => setTokenFor(p)}
            />
          ))}
        </div>
        <div className="mt-4 flex items-center justify-between px-4 pb-1">
          <div className="text-xs text-slate-500">项目</div>
          <Button
            variant="primary"
            className="!px-2 !py-1 !text-xs"
            disabled={!anyConnected}
            title={anyConnected ? undefined : "先连接一家平台"}
            onClick={() => setAdding(true)}
          >
            ＋ 登记
          </Button>
        </div>
        <div className="flex-1 space-y-1 overflow-y-auto px-3 pb-3">
          {targets.data?.map((t) => {
            const d = latest.get(t.id);
            const tone: Tone = !d || d.state === "queued" ? "slate" : d.state === "error" ? "rose" : d.state === "ready" ? "emerald" : "sky";
            return (
              <button key={t.id} onClick={() => pick(t.id)} className={`w-full rounded-lg px-3 py-2 text-left transition ${selected === t.id ? "bg-slate-800" : "hover:bg-slate-800/60"}`}>
                <div className="flex items-center gap-2">
                  <Badge tone={PROVIDER[t.provider].tone}>{PROVIDER[t.provider].label}</Badge>
                  <span className="min-w-0 truncate text-xs text-slate-200">{t.name}</span>
                  {d && <span className={`ml-auto h-1.5 w-1.5 shrink-0 rounded-full ${DOT[tone]}`} />}
                </div>
                <div className="mt-1 truncate text-[11px] text-slate-500">
                  {[t.repo, t.branch].filter(Boolean).join(" · ") || "没有关联仓库"}
                  {d && ` · ${DEPLOY_STATE[d.state].label}`}
                </div>
              </button>
            );
          })}
          {targets.isSuccess && targets.data.length === 0 && (
            <div className="rounded-lg border border-dashed border-slate-800 p-3 text-xs leading-5 text-slate-600">
              {anyConnected ? "还没有登记项目。点「登记」从平台上选。" : "先在上面填入 Vercel 或 Cloudflare 的 API 令牌。"}
            </div>
          )}
        </div>
      </aside>

      <section className="min-w-0 flex-1 overflow-y-auto">
        {selected == null ? (
          <div className="p-8">
            <Empty
              title="选一个项目"
              desc="左边选一个登记过的项目看部署、发起部署或回滚；还没登记的话先连接平台"
            />
          </div>
        ) : (
          <TargetDetail id={selected} onRemoved={clear} />
        )}
      </section>

      {tokenFor && <TokenDialog platform={tokenFor} onClose={() => setTokenFor(null)} />}
      {adding && <AddTargetDialog onClose={() => setAdding(false)} onAdded={pick} />}
    </div>
  );
}

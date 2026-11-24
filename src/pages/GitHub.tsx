import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  githubConnect,
  githubDisconnect,
  githubIssues,
  githubPrs,
  githubRepos,
  githubStatus,
  taskCreateIssue,
} from "../lib/api";
import type { GhIssue } from "../lib/types";
import { Badge, Card, PageHeader } from "../components/ui";

export default function GitHub() {
  const qc = useQueryClient();
  const [token, setToken] = useState("");
  const [tab, setTab] = useState<"issues" | "prs">("issues");
  const [selectedRepo, setSelectedRepo] = useState<string | null>(() =>
    localStorage.getItem("aa.repo"),
  );
  const [notice, setNotice] = useState<string | null>(null);

  const status = useQuery({
    queryKey: ["ghStatus"],
    queryFn: githubStatus,
    retry: false,
  });
  const connect = useMutation({
    mutationFn: () => githubConnect(token),
    onSuccess: (u) => {
      setToken("");
      setNotice(`已连接 ${u.login}`);
      qc.invalidateQueries({ queryKey: ["ghStatus"] });
    },
    onError: (e) => setNotice(String(e)),
  });
  const disconnect = useMutation({
    mutationFn: githubDisconnect,
    onSuccess: () => {
      setSelectedRepo(null);
      localStorage.removeItem("aa.repo");
      qc.invalidateQueries({ queryKey: ["ghStatus"] });
    },
  });

  const connected = !!status.data;
  const repos = useQuery({
    queryKey: ["ghRepos"],
    queryFn: githubRepos,
    enabled: connected,
  });

  const [owner, repo] = (selectedRepo ?? "/").split("/");
  const issues = useQuery({
    queryKey: ["ghIssues", selectedRepo],
    queryFn: () => githubIssues(owner, repo),
    enabled: connected && !!selectedRepo && tab === "issues",
  });
  const prs = useQuery({
    queryKey: ["ghPrs", selectedRepo],
    queryFn: () => githubPrs(owner, repo),
    enabled: connected && !!selectedRepo && tab === "prs",
  });

  const createTask = useMutation({
    mutationFn: (i: GhIssue) =>
      taskCreateIssue({
        title: i.title,
        repo: selectedRepo!,
        issueNumber: i.number,
        issueUrl: i.htmlUrl,
      }),
    onSuccess: (task) => {
      setNotice(`已创建任务「${task.title}」，启动后 Agent 会在独立的工作树里实现它`);
      qc.invalidateQueries({ queryKey: ["tasks"] });
    },
    onError: (e) => setNotice(String(e)),
  });

  const selectRepo = (fullName: string) => {
    setSelectedRepo(fullName);
    localStorage.setItem("aa.repo", fullName);
  };

  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader
        title="GitHub"
        desc="Issue / PR 看板与 Agent 协作的入口"
        actions={
          connected ? (
            <div className="flex items-center gap-3">
              {status.data?.avatarUrl && (
                <img
                  src={status.data.avatarUrl}
                  alt=""
                  className="h-7 w-7 rounded-full border border-slate-700"
                />
              )}
              <span className="text-sm text-slate-300">{status.data?.login}</span>
              <button
                onClick={() => disconnect.mutate()}
                className="rounded-lg border border-slate-700 px-2.5 py-1 text-xs text-slate-400 hover:border-rose-600 hover:text-rose-400"
              >
                断开
              </button>
            </div>
          ) : undefined
        }
      />

      {notice && (
        <div className="mb-4 flex items-center justify-between rounded-lg border border-sky-700/50 bg-sky-950/40 px-4 py-2 text-sm text-sky-300">
          <span>{notice}</span>
          <button onClick={() => setNotice(null)} className="text-sky-500 hover:text-sky-300">
            ✕
          </button>
        </div>
      )}

      {!connected ? (
        <Card className="p-5">
          <h2 className="text-sm font-medium text-slate-300">连接 GitHub</h2>
          <p className="mt-1 text-xs leading-5 text-slate-500">
            使用 Personal Access Token（推荐 fine-grained，仅授予目标仓库的 issues / pull
            requests / contents 权限）。Token 仅存本机 Keychain，不离开设备。
          </p>
          <div className="mt-4 flex gap-3">
            <input
              type="password"
              value={token}
              onChange={(e) => setToken(e.currentTarget.value)}
              placeholder="github_pat_…"
              className="flex-1 rounded-lg border border-slate-700 bg-slate-950 px-3 py-2 text-sm text-slate-200 placeholder:text-slate-600 focus:border-sky-600 focus:outline-none"
            />
            <button
              onClick={() => connect.mutate()}
              disabled={connect.isPending || !token.trim()}
              className="rounded-lg bg-sky-600 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-500 disabled:cursor-not-allowed disabled:opacity-50"
            >
              {connect.isPending ? "连接中…" : "连接"}
            </button>
          </div>
          {connect.isError && (
            <p className="mt-2 text-xs text-rose-400">{String(connect.error)}</p>
          )}
        </Card>
      ) : !selectedRepo ? (
        <div>
          <h2 className="mb-3 text-sm font-medium text-slate-300">选择仓库</h2>
          {repos.isLoading ? (
            <Empty text="加载仓库列表…" />
          ) : repos.isError ? (
            <Empty text={String(repos.error)} />
          ) : (
            <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
              {repos.data?.map((r) => (
                <button
                  key={r.id}
                  onClick={() => selectRepo(r.fullName)}
                  className="rounded-xl border border-slate-800 bg-slate-900/60 p-4 text-left transition hover:border-sky-600"
                >
                  <div className="flex items-center gap-2">
                    <span className="truncate text-sm font-medium text-white">
                      {r.fullName}
                    </span>
                    {r.private && <Badge tone="amber">私有</Badge>}
                  </div>
                  <p className="mt-1 line-clamp-2 text-xs leading-5 text-slate-500">
                    {r.description ?? "无描述"}
                  </p>
                  <div className="mt-2 flex items-center gap-3 text-xs text-slate-500">
                    <span>🐞 {r.openIssues}</span>
                    {r.updatedAt && <span>{r.updatedAt.slice(0, 10)}</span>}
                  </div>
                </button>
              ))}
            </div>
          )}
        </div>
      ) : (
        <div>
          <div className="mb-4 flex items-center gap-3">
            <button
              onClick={() => selectRepo("")}
              className="rounded-lg border border-slate-700 px-2.5 py-1 text-xs text-slate-400 hover:border-sky-600 hover:text-white"
            >
              ← 换仓库
            </button>
            <h2 className="text-sm font-medium text-white">{selectedRepo}</h2>
            <div className="ml-auto flex rounded-lg border border-slate-700 p-0.5 text-xs">
              {(["issues", "prs"] as const).map((t) => (
                <button
                  key={t}
                  onClick={() => setTab(t)}
                  className={`rounded-md px-3 py-1 transition ${
                    tab === t ? "bg-slate-800 text-white" : "text-slate-400 hover:text-slate-200"
                  }`}
                >
                  {t === "issues" ? "Issues" : "Pull Requests"}
                </button>
              ))}
            </div>
          </div>

          {tab === "issues" ? (
            issues.isLoading ? (
              <Empty text="加载 Issues…" />
            ) : issues.isError ? (
              <Empty text={String(issues.error)} />
            ) : !issues.data?.length ? (
              <Empty text="没有 Issue" />
            ) : (
              <Card className="divide-y divide-slate-800/70">
                {issues.data.map((i) => (
                  <div key={i.number} className="flex items-center gap-3 px-4 py-3">
                    <Badge tone={i.state === "open" ? "emerald" : "slate"}>
                      {i.state === "open" ? "开放" : "关闭"}
                    </Badge>
                    <button
                      onClick={() => openUrl(i.htmlUrl)}
                      className="min-w-0 flex-1 truncate text-left text-sm text-slate-200 hover:text-sky-400"
                    >
                      <span className="text-slate-500">#{i.number}</span> {i.title}
                    </button>
                    <div className="hidden shrink-0 items-center gap-1.5 md:flex">
                      {i.labels.slice(0, 3).map((l) => (
                        <Badge key={l} tone="violet">
                          {l}
                        </Badge>
                      ))}
                    </div>
                    <span className="hidden w-20 shrink-0 truncate text-right text-xs text-slate-500 lg:block">
                      {i.author}
                    </span>
                    <button
                      onClick={() => createTask.mutate(i)}
                      disabled={createTask.isPending && createTask.variables?.number === i.number}
                      className="shrink-0 rounded-md border border-slate-700 px-2 py-1 text-xs text-slate-300 transition hover:border-sky-600 hover:text-white disabled:opacity-50"
                    >
                      {createTask.isPending && createTask.variables?.number === i.number
                        ? "创建中…"
                        : "转任务"}
                    </button>
                  </div>
                ))}
              </Card>
            )
          ) : prs.isLoading ? (
            <Empty text="加载 PR…" />
          ) : prs.isError ? (
            <Empty text={String(prs.error)} />
          ) : !prs.data?.length ? (
            <Empty text="没有 Pull Request" />
          ) : (
            <Card className="divide-y divide-slate-800/70">
              {prs.data.map((p) => (
                <div key={p.number} className="flex items-center gap-3 px-4 py-3">
                  <Badge tone={p.state === "open" ? (p.draft ? "amber" : "sky") : "slate"}>
                    {p.state === "open" ? (p.draft ? "草稿" : "开放") : p.state === "closed" ? "关闭" : "已合并"}
                  </Badge>
                  <button
                    onClick={() => openUrl(p.htmlUrl)}
                    className="min-w-0 flex-1 truncate text-left text-sm text-slate-200 hover:text-sky-400"
                  >
                    <span className="text-slate-500">#{p.number}</span> {p.title}
                  </button>
                  <span className="hidden shrink-0 font-mono text-xs text-slate-500 md:block">
                    {p.headRef} → {p.baseRef}
                  </span>
                  <span className="hidden w-20 shrink-0 truncate text-right text-xs text-slate-500 lg:block">
                    {p.author}
                  </span>
                </div>
              ))}
            </Card>
          )}
        </div>
      )}
    </div>
  );
}

function Empty({ text }: { text: string }) {
  return (
    <Card className="border-dashed p-8 text-center text-sm text-slate-500">{text}</Card>
  );
}

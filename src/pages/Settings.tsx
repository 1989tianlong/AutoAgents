import type { ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { githubStatus } from "../lib/api";
import { Badge, Card, PageHeader } from "../components/ui";

export default function Settings() {
  const { data: ghUser } = useQuery({
    queryKey: ["ghStatus"],
    queryFn: githubStatus,
    retry: false,
  });
  return (
    <div className="mx-auto max-w-3xl p-8">
      <PageHeader title="设置" desc="大部分选项将随后续里程碑逐步生效" />

      <Section title="通用">
        <Row label="界面语言" desc="当前：简体中文" />
        <Row label="主题" desc="跟随系统（暂不支持切换）" />
      </Section>

      <Section title="连接">
        <Row label="局域网发现" desc="mDNS 自动发现 agentd（M2）" />
        <Row label="中继 / Tailscale" desc="跨网访问家庭电脑与服务器（M2）" />
      </Section>

      <Section title="GitHub">
        <Row
          label="账号"
          desc={ghUser ? `已连接：${ghUser.login}` : "未连接（在 GitHub 页连接）"}
          badge={
            ghUser ? <Badge tone="emerald">已连接</Badge> : <Badge tone="slate">未连接</Badge>
          }
        />
        <Row label="凭据存储" desc="PAT 加密存于本机 Keychain（com.tianlong.autoagents）" />
      </Section>

      <Section title="部署">
        <Row label="Vercel / Cloudflare" desc="未连接（M3）" />
      </Section>

      <Section title="安全">
        <Row
          label="写操作人工确认"
          desc="Agent 的 push / merge / 评论默认需人工确认"
          badge={<Badge tone="emerald">已开启</Badge>}
        />
        <Row label="密钥存储" desc="Token 与 SSH key 仅存本机 Keychain" />
      </Section>
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

function Row({ label, desc, badge }: { label: string; desc: string; badge?: ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-4 px-5 py-3.5">
      <div>
        <div className="text-sm text-slate-200">{label}</div>
        <div className="text-xs text-slate-500">{desc}</div>
      </div>
      {badge}
    </div>
  );
}

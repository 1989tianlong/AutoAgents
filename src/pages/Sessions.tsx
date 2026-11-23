import { Link } from "react-router-dom";
import { Empty, PageHeader } from "../components/ui";

export default function Sessions() {
  return (
    <div className="mx-auto max-w-6xl p-8">
      <PageHeader title="会话" desc="应用内直接与 CLI Agent 对话，无需终端" />
      <Empty
        title="还没有会话"
        desc="前往 Agents 页选择一个 L2/L3 能力的 CLI Agent（如 Claude CLI、Codex CLI）开启托管会话。托管会话将在 M1 后续版本开放。"
      >
        <Link
          to="/agents"
          className="rounded-lg bg-sky-600 px-4 py-2 text-sm font-medium text-white transition hover:bg-sky-500"
        >
          去 Agents
        </Link>
      </Empty>
    </div>
  );
}

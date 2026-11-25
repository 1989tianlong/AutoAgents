/** 活动流（事件表）在界面上的归类与文案。kind 用 `模块.动作` 命名，新增事件时在这里补一条文案即可。 */

export interface KindGroup {
  /** 对应 kind 的前缀；null 表示全部 */
  key: string | null;
  label: string;
}

export const KIND_GROUPS: KindGroup[] = [
  { key: null, label: "全部" },
  { key: "session", label: "会话" },
  { key: "node", label: "机器" },
  { key: "task", label: "任务" },
  { key: "deploy", label: "部署" },
  { key: "github", label: "GitHub" },
  { key: "relay", label: "中继" },
];

const KIND_LABEL: Record<string, string> = {
  "node.paired": "机器接入",
  "node.online": "机器上线",
  "node.offline": "机器离线",
  "node.removed": "机器移除",
  "node.authorize": "授权设备",
  "relay.configured": "中继已设置",
  "relay.cleared": "中继已清除",
  "session.started": "开始一轮",
  "session.finished": "完成一轮",
  "session.failed": "一轮失败",
  "github.connected": "GitHub 已连接",
  "github.disconnected": "GitHub 已断开",
  "task.created": "新建任务",
  "task.started": "任务启动",
  "task.changes": "改动待确认",
  "task.push": "推送提交",
  "task.pr": "创建 PR",
  "task.merged": "PR 已合并",
  "task.failed": "任务失败",
  "task.status": "任务状态",
  "task.deleted": "删除任务",
  "deploy.connected": "连接平台",
  "deploy.disconnected": "断开平台",
  "deploy.discover": "取项目清单",
  "deploy.target": "登记项目",
  "deploy.refresh": "同步状态",
  "deploy.trigger": "发起部署",
  "deploy.retry": "重做部署",
  "deploy.cancel": "取消部署",
  "deploy.promote": "升为生产",
  "deploy.rollback": "回滚",
  "deploy.ready": "部署成功",
  "deploy.failed": "部署失败",
  "deploy.health": "健康检查",
  "deploy.guard": "守护",
  "deploy.repair": "建修复任务",
};

export function kindLabel(kind: string): string {
  return KIND_LABEL[kind] ?? kind;
}

/** 事件标签的色调：出错用红、需留意用黄、上线用绿，其余中性 */
export function kindTone(kind: string, level: string): "emerald" | "amber" | "rose" | "slate" {
  if (level === "error") return "rose";
  if (level === "warning") return "amber";
  if (kind === "node.online" || kind.endsWith(".connected")) return "emerald";
  return "slate";
}

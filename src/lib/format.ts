/** 界面上用的格式化小工具 */

/** 字节数 → 人读的单位 */
export function fmtBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value.toFixed(i === 0 ? 0 : value >= 100 ? 0 : 1)} ${units[i]}`;
}

/** 运行时长（秒）→ `3 天 4 小时` */
export function fmtUptime(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时`;
  if (hours > 0) return `${hours} 小时 ${minutes} 分`;
  return `${minutes} 分 ${s % 60} 秒`;
}

/** 剩余秒数 → `08:32` */
export function fmtCountdown(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${String(m).padStart(2, "0")}:${String(r).padStart(2, "0")}`;
}

/** 距现在多久：`刚刚` / `5 分钟前` / `3 小时前` / 日期 */
export function fmtAgo(input: string | number | null | undefined): string {
  if (input == null) return "—";
  const ms = typeof input === "number" ? input : Date.parse(input.replace(" ", "T") + "Z");
  if (!Number.isFinite(ms)) return String(input);
  const diff = Date.now() - ms;
  if (diff < 0) return new Date(ms).toLocaleString();
  const minutes = Math.floor(diff / 60000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days} 天前`;
  return new Date(ms).toLocaleDateString();
}

/** 事件时间戳（SQLite 的 UTC `YYYY-MM-DD HH:MM:SS`）→ 本地 `MM-DD HH:MM` */
export function fmtTime(input: string): string {
  const ms = Date.parse(input.replace(" ", "T") + "Z");
  return Number.isFinite(ms) ? new Date(ms).toLocaleString() : input;
}

/** 美元费用：很小的时候也不显示成 0 */
export function fmtUsd(cost: number): string {
  if (cost === 0) return "$0";
  if (cost < 0.01) return `$${cost.toFixed(5)}`;
  return `$${cost.toFixed(3)}`;
}

/** 取一段文本的开头，中间用省略号 */
export function short(text: string, head = 18, tail = 6): string {
  return text.length <= head + tail + 1 ? text : `${text.slice(0, head)}…${text.slice(-tail)}`;
}

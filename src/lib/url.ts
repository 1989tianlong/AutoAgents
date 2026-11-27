import { openUrl } from "@tauri-apps/plugin-opener";

/** 仅允许通过系统浏览器打开 http(s) 链接，阻断 javascript:/file: 等危险协议 */
export function openSafeUrl(raw: string | null | undefined): void {
  if (!raw) return;
  try {
    const parsed = new URL(raw);
    if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
      return;
    }
    void openUrl(parsed.toString()).catch(() => {});
  } catch {
    // 忽略非法 URL
  }
}

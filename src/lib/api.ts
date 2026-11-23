import { invoke } from "@tauri-apps/api/core";
import type { AppInfo, DiscoveredAgent } from "./types";

export const getAppInfo = () => invoke<AppInfo>("app_info");

/** 扫描本机 PATH 与 /Applications，发现已安装的 Agent */
export const discoverAgents = () => invoke<DiscoveredAgent[]>("discover_agents");

export type AgentKind = "cli" | "desktop";
export type AdapterLevel = "L1" | "L2" | "L3";

/** 本机发现的一个 Agent（对应 Rust 端 discovery::DiscoveredAgent） */
export interface DiscoveredAgent {
  id: string;
  name: string;
  kind: AgentKind;
  level: AdapterLevel;
  adapter: string;
  path: string;
  version: string | null;
}

export interface AppInfo {
  name: string;
  version: string;
  os: string;
  arch: string;
  hostname: string;
}

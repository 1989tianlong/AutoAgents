import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { NodeStatusEvent } from "./types";

/** 机器上线、掉线、接入、移除时 Rust 端会推 `node-event`：在整个应用里订阅一次，
 *  让依赖机器清单的查询（机器、各机器上的 Agent、中继状态）统统刷新。 */
export function useNodeEvents() {
  const qc = useQueryClient();
  useEffect(() => {
    let un: UnlistenFn | undefined;
    listen<NodeStatusEvent>("node-event", () => {
      for (const key of ["machines", "agentsAll", "relayStatus"]) {
        qc.invalidateQueries({ queryKey: [key] });
      }
    })
      .then((f) => (un = f))
      .catch(() => {});
    return () => un?.();
  }, [qc]);
}

import { useEffect, useRef } from "react";
import { uxPluginRecords, useUxPluginRecords } from "@/contrib/plugins-store";
import { uxScanSettled } from "@/contrib/runtime-door";

/** A UX plugin's page in a window of its own opens on that page, with its
    data calls going to the node it was opened from. The shell opens the
    window at `index.html?view=plugin&plugin=<id>&node=<did>`; any other
    address is an ordinary window. */
export type PluginWindowStart = { pluginId: string; nodeDid: string | null };

export function pluginWindowStart(search: string): PluginWindowStart | null {
  const params = new URLSearchParams(search);
  if (params.get("view") !== "plugin") return null;
  const pluginId = params.get("plugin");
  if (!pluginId) return null;
  return { pluginId, nodeDid: params.get("node") || null };
}

/** Closes the window once its plugin, after running here, is turned off,
    removed or fails. A plugin missing while a scan reloads it is not gone,
    so the check waits for the scan; one that never loaded leaves the page's
    own "no page" state up instead. */
export function useClosesWhenPluginGone(pluginId: string, close: () => void) {
  const loaded = useUxPluginRecords()[pluginId]?.status === "loaded";
  const ran = useRef(false);
  const closeRef = useRef(close);
  closeRef.current = close;
  useEffect(() => {
    if (loaded) {
      ran.current = true;
      return;
    }
    if (!ran.current) return;
    let current = true;
    void uxScanSettled().then(() => {
      if (current && uxPluginRecords()[pluginId]?.status !== "loaded") {
        closeRef.current();
      }
    });
    return () => {
      current = false;
    };
  }, [loaded, pluginId]);
}

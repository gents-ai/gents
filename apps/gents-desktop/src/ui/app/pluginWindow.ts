import { useEffect, useRef, useState } from "react";
import { uxPluginRecords, useUxPluginRecords } from "@/contrib/plugins-store";
import { uxScanSettled } from "@/contrib/runtime-door";
import {
  inNativeShell,
  listenToWindow,
  PLUGIN_NAVIGATE_EVENT,
  PLUGIN_WINDOW_EVENT,
  pluginWindowLabel,
  windowLabel,
  windowOpen,
  type PluginNavigation,
  type PluginWindowChange,
} from "../../lib/nativeShell";
import { navigate, parseRoute, type Route } from "../lib/router";

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

/** Whether plugin `pluginId` has a window of its own open, as the main
    window follows it: asked once, then told of each open and close. What it
    was told wins over an answer that arrives after it. `follow` is false
    where no such window can exist (a browser, a phone, the window itself). */
export function usePluginWindowOpen(pluginId: string, follow: boolean): boolean {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    setOpen(false);
    if (!follow) return;
    let live = true;
    let told = false;
    const stop = listenToWindow<PluginWindowChange>(PLUGIN_WINDOW_EVENT, (change) => {
      if (!live || change.pluginId !== pluginId) return;
      told = true;
      setOpen(change.open);
    });
    void stop
      .then(() => windowOpen(pluginWindowLabel(pluginId)))
      .then(
        (now) => live && !told && setOpen(now),
        () => {},
      );
    return () => {
      live = false;
      void stop.then(
        (unlisten) => unlisten(),
        () => {},
      );
    };
  }, [pluginId, follow]);
  return follow && open;
}

/** The main window goes where a plugin's page, in a window of its own, asked
    to go: that window shows the page and nothing else, so the shell hands
    the place over and brings the main window forward. The path is read as
    any of the app's own; one that names nothing opens the default place. */
export function useNavigationFromPluginWindows(go: (to: Route) => void = navigate) {
  useEffect(() => {
    if (!inNativeShell() || windowLabel() !== "main") return;
    const stop = listenToWindow<PluginNavigation>(PLUGIN_NAVIGATE_EVENT, (asked) => {
      if (typeof asked?.path === "string") go(parseRoute(asked.path));
    });
    return () =>
      void stop.then(
        (unlisten) => unlisten(),
        () => {},
      );
  }, [go]);
}

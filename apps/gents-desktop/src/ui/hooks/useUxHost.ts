/* The app's one wiring point for UX plugins. Installs the UxHost (what a
   plugin may read and call), opens every door, and feeds the update
   stream into the plugin tap. Called from AppHost once the shell is ready;
   the shell reference is kept current on every render through a ref so
   plugins always read the live snapshot without re-installing. */
import { useEffect, useRef } from "react";
import type { Shell, ShellBridge } from "./useShell";
import type { Route } from "@/lib/router";
import { navigate, useRoute } from "@/lib/router";
import { call } from "@/screens/agent/bridgeCall";
import { pickDirectory } from "@/lib/pickDirectory";
import { revealInFolder } from "../../lib/shellPlatform";
import { discoverBundledUxPlugins } from "@/contrib/bundled";
import { emitClientUpdated } from "@/contrib/events";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import { scanRuntimeUxPlugins } from "@/contrib/runtime-door";
import type { DesktopClientUpdatedEvent } from "@source-inc/gents-desktop-client";

export function buildUxHost(shell: () => Shell, route: () => Route): UxHost {
  return {
    shell: {
      route,
      navigate,
      deployments: () => shell().deployments,
      selectedDeployment: () => shell().selectedDeployment,
      selectedSessionId: () => shell().selectedSessionId ?? null,
      send: async (content) => {
        const s = shell();
        await s.sendMessage(content, s.selectedBehaviorId ?? null);
      },
    },
    bridge: (command, args) => call(command, args),
    plugin: (pack, name, input) =>
      call<{ output: unknown }>("desktop_plugin_call", { pack, name, input }).then(
        (r) => r.output,
      ),
    os: {
      openExternal: async (url) => {
        try {
          await call("desktop_open_external_url", { url });
          return true;
        } catch {
          return false;
        }
      },
      revealPath: async (path) => {
        try {
          await revealInFolder(path);
          return true;
        } catch {
          return false;
        }
      },
      pickDirectory: (options) => pickDirectory(options ?? {}).catch(() => null),
    },
  };
}

let opened = false;

/* idempotent: the doors open once per webview, however often the shell
   re-renders or HMR re-evaluates the caller */
export function openUxDoors(): void {
  if (opened) return;
  opened = true;
  discoverBundledUxPlugins();
  void scanRuntimeUxPlugins();
}

export function useUxHost(
  shell: Shell,
  listenToUpdates: ShellBridge["listenToUpdates"],
): void {
  const route = useRoute();
  const shellRef = useRef(shell);
  const routeRef = useRef(route);
  shellRef.current = shell;
  routeRef.current = route;
  useEffect(() => {
    installUxHost(
      buildUxHost(
        () => shellRef.current,
        () => routeRef.current,
      ),
    );
    openUxDoors();
  }, []);
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void Promise.resolve(
      listenToUpdates((event: DesktopClientUpdatedEvent) => emitClientUpdated(event)),
    ).then((stop: () => void) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [listenToUpdates]);
}

/** test seam */
export function resetUxDoors(): void {
  opened = false;
}

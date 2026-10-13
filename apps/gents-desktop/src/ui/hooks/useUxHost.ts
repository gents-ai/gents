/* The app's one wiring point for UX plugins. Installs the UxHost (what a
   plugin may read and call), opens every door, and feeds the update
   stream into the plugin tap. Reads go straight to the app's stores
   (`getState()`), so a plugin always sees the live snapshot and nothing
   here re-renders; the route is kept current through a ref. Called from
   App's reactions leaf. */
import { useEffect, useRef } from "react";
import type { DesktopApp, DesktopBridge } from "../../hooks/desktopApp";
import { nodeOf } from "../../hooks/fleetStore";
import { fleetNodes } from "@/lib/scope";
import { navigate, useRoute, type Route } from "@/lib/router";
import { call } from "@/screens/agent/bridgeCall";
import { pickDirectory } from "@/lib/pickDirectory";
import { revealInFolder } from "../../lib/shellPlatform";
import { discoverBundledUxPlugins } from "@/contrib/bundled";
import { emitClientUpdated } from "@/contrib/events";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import { scanRuntimeUxPlugins } from "@/contrib/runtime-door";
import type { DesktopClientUpdatedEvent } from "@source-inc/gents-desktop-client";

/* the gate on ctx.send: a plugin posts a turn in the user's name, so it
   must follow the user's own gesture (a click or a key press), never a
   timer, an effect or a directive's mount, or the model could prompt
   itself through a plugin it addressed */
function withinUserGesture(): boolean {
  const activation = (
    navigator as Navigator & { userActivation?: { isActive: boolean } }
  ).userActivation;
  return activation?.isActive === true;
}

export function buildUxHost(app: DesktopApp, route: () => Route): UxHost {
  const selection = () => app.stores.selection.getState();
  const fleet = () => app.stores.fleet.getState();
  /* the selected node, or the first while nothing is selected: the same
     rule useSelectedNode applies */
  const selectedNode = () =>
    nodeOf(fleet(), selection().agentDid) ?? fleetNodes(fleet())[0] ?? null;
  return {
    shell: {
      route,
      navigate,
      deployments: () => fleetNodes(fleet()),
      selectedDeployment: selectedNode,
      selectedSessionId: () => selection().sessionId,
      send: async (content) => {
        if (selection().sessionId === null) {
          throw new Error("ctx.send: no session is selected");
        }
        if (!withinUserGesture()) {
          throw new Error(
            "ctx.send: only from a user gesture (a click or a key press)",
          );
        }
        await app.actions.sendMessage(content, selection().behaviorId);
      },
    },
    /* the bridge takes its fields under `request`, as desktop_pack_install does */
    plugin: (listing, name, input) =>
      call<{ output: unknown }>("desktop_plugin_call", {
        request: { ux: listing, name, input },
      }).then((r) => r.output),
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
  app: DesktopApp,
  listenToUpdates: DesktopBridge["listenToUpdates"],
): void {
  const route = useRoute();
  const routeRef = useRef(route);
  routeRef.current = route;
  useEffect(() => {
    installUxHost(buildUxHost(app, () => routeRef.current));
    openUxDoors();
  }, [app]);
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

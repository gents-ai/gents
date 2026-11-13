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
import {
  confirmPluginMessage,
  pickDirectory,
  revealInFolder,
} from "../../lib/nativeShell";
import { discoverBundledUxPlugins } from "@/contrib/bundled";
import { emitClientUpdated } from "@/contrib/events";
import { installUxHost, type UxHost } from "@/contrib/plugin";
import { createPluginNotifier } from "@/lib/notify/plugins";
import { followOtherWindowsDecisions } from "@/contrib/plugins-store";
import {
  configureUxDoors,
  followOtherWindowsPackChanges,
  followSelectedUxNode,
  rescanRuntimeUxPluginsSoon,
  scanRuntimeUxPlugins,
} from "@/contrib/runtime-door";
import type { DesktopClientUpdatedEvent } from "@source-inc/gents-desktop-client";

/* the gate on ctx.send and os.openExternal: a plugin posts a turn in the
   user's name, or carries data out of the app in a URL, so either must
   follow the user's own gesture (a click or a key press), never a timer,
   an effect or a directive's mount; otherwise the model could prompt itself
   through a plugin it addressed, or a shared pack send data out unasked */
function withinUserGesture(): boolean {
  const activation = (
    navigator as Navigator & { userActivation?: { isActive: boolean } }
  ).userActivation;
  return activation?.isActive === true;
}

/* route changes arrive through useUxHost's effect; stores notify on their own */
const routeListeners = new Set<() => void>();
function notifyRouteChanged() {
  routeListeners.forEach((listener) => listener());
}

/* one per window, so a plugin's rate holds across the host's rebuilds */
let pluginNotifier: ReturnType<typeof createPluginNotifier> | null = null;

export function buildUxHost(app: DesktopApp, route: () => Route): UxHost {
  let confirmingMessage = false;
  const selection = () => app.stores.selection.getState();
  const fleet = () => app.stores.fleet.getState();
  /* the selected node, or the first while nothing is selected: the same
     rule useSelectedNode applies */
  const selectedNode = () =>
    nodeOf(fleet(), selection().nodeDid) ?? fleetNodes(fleet())[0] ?? null;
  return {
    shell: {
      route,
      navigate,
      deployments: () => fleetNodes(fleet()),
      selectedDeployment: selectedNode,
      selectedSessionId: () => selection().sessionId,
      send: async (content, caller) => {
        const target = selection();
        if (target.sessionId === null) {
          throw new Error("ctx.send: no session is selected");
        }
        if (!withinUserGesture()) {
          throw new Error(
            "ctx.send: only from a user gesture (a click or a key press)",
          );
        }
        if (confirmingMessage)
          throw new Error("ctx.send: a message is already awaiting confirmation");
        confirmingMessage = true;
        try {
          if (!(await confirmPluginMessage(caller.name, content))) {
            throw new Error("ctx.send: message was not confirmed");
          }
          const current = selection();
          if (
            current.nodeDid !== target.nodeDid ||
            current.sessionId !== target.sessionId ||
            current.agentDid !== target.agentDid
          ) {
            throw new Error(
              "ctx.send: the selected chat changed; request the message again",
            );
          }
          await app.actions.sendMessage(content, target.agentDid);
        } finally {
          confirmingMessage = false;
        }
      },
    },
    /* through the client's adapter, which sends the fields under `request` */
    plugin: (listing, name, input) =>
      app.actions.callPackPlugin({ ux: listing, name, input }).then((r) => r.output),
    data: {
      call: (listing, nodeDid, surface, tool, args) =>
        app.actions.callUxData({ ux: listing, nodeDid, surface, tool, args }),
      watch: (listing, nodeDid, surface, tool, args, listener) =>
        app.actions.watchUxData(
          { ux: listing, nodeDid, surface, tool, args },
          listener,
        ),
      surfaces: (listing, nodeDid) =>
        app.actions
          .listUxDataSurfaces({ ux: listing, nodeDid })
          .then((r) => r.surfaces),
    },
    onShellChange: (listener) => {
      const offs = [
        app.stores.selection.subscribe(listener),
        app.stores.fleet.subscribe(listener),
      ];
      routeListeners.add(listener);
      return () => {
        offs.forEach((off) => off());
        routeListeners.delete(listener);
      };
    },
    os: {
      openExternal: async (url) => {
        if (!withinUserGesture()) return false;
        try {
          await app.actions.openExternalUrl(url);
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
    notify: (caller, request) =>
      (pluginNotifier ??= createPluginNotifier())(caller, request),
  };
}

let opened = false;
let following: Array<() => void> = [];

/* idempotent: the doors open once per webview, however often the shell
   re-renders or HMR re-evaluates the caller */
export function openUxDoors(app: DesktopApp): void {
  if (opened) return;
  opened = true;
  /* page documents are read from the selected node, by the same rule the
     plugin host's selectedDeployment applies */
  const nodeDid = () => {
    const fleet = app.stores.fleet.getState();
    const selected =
      nodeOf(fleet, app.stores.selection.getState().nodeDid) ?? fleetNodes(fleet)[0];
    return selected?.nodeDid ?? null;
  };
  configureUxDoors({
    list: (node) => app.actions.listUxPlugins(node),
    source: (id, node) => app.actions.readUxPluginSource(id, node),
    node: nodeDid,
  });
  discoverBundledUxPlugins();
  void scanRuntimeUxPlugins();
  /* another node's pages replace this one's when the selection moves */
  const followNode = () => void followSelectedUxNode().catch(() => undefined);
  following = [
    followOtherWindowsDecisions(),
    followOtherWindowsPackChanges(),
    app.stores.selection.subscribe(followNode),
    app.stores.fleet.subscribe(followNode),
  ];
}

const showsPages = (route: Route): boolean =>
  route.name === "plugin" || route.name === "packs";

export function useUxHost(
  app: DesktopApp,
  listenToUpdates: DesktopBridge["listenToUpdates"],
): void {
  const route = useRoute();
  const routeRef = useRef(route);
  routeRef.current = route;
  useEffect(() => {
    notifyRouteChanged();
    if (showsPages(route)) rescanRuntimeUxPluginsSoon();
  }, [route]);
  useEffect(() => {
    installUxHost(buildUxHost(app, () => routeRef.current));
    openUxDoors(app);
  }, [app]);
  /* a pack reinstalled from the CLI changes nothing this window hears of;
     coming back to the window is when its user expects the new pages */
  useEffect(() => {
    const onFocus = () => rescanRuntimeUxPluginsSoon();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void Promise.resolve(
      listenToUpdates((event: DesktopClientUpdatedEvent) => {
        emitClientUpdated(event);
        /* An agent editing a page document is client activity, and so is
           every streamed token. Only a screen that shows pages follows it, so
           a chat turn adds no bridge calls (the mobile performance gate). */
        if (showsPages(routeRef.current)) rescanRuntimeUxPluginsSoon();
      }),
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
  following.splice(0).forEach((stop) => stop());
}

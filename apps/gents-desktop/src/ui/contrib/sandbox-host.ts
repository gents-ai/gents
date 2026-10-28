/* The shell's side of a plugin's sandbox: one frame per enabled plugin,
   one MessagePort, and the port is the plugin's identity. Everything the
   frame asks for is answered through the host-side UxContext built for
   this plugin (scoped storage, the pack of its listing, the declared
   areas), so the message carries no identity and could not claim one. A
   plugin's shell-rendered contributions arrive as data and are registered
   here on its behalf; its pages draw in the frame, which the stage
   positions over the viewport that shows them. */
import { createElement } from "react";
import { toast } from "sonner";

import type { UxContext, UxHost } from "./plugin";
import { uxHost } from "./plugin";
import { onUxEvent } from "./events";
import { patchUxPlugin, type UxDeclaredContributions } from "./plugins-store";
import { unloadUxPlugin } from "./activate";
import { frameNonce, loadRuntimeAssets, sandboxDocument } from "./frame-document";
import { placeFrame, stageElement } from "./frame-stage";
import { FrameViewport } from "./react/frame-viewport";
import { DirectiveChip, HeaderActionButton } from "./react/shell-widgets";
import { pluginIcon } from "./icons";
import type { NavItem } from "@/app/navRegistry";
import type { Route } from "@/lib/router";

/* the row lights for the route it points at: by name, and by the field
   that tells two rows of one name apart; a node is not one of them */
function routeMatches(to: Record<string, unknown>, route: Route): boolean {
  if (route.name !== to.name) return false;
  switch (route.name) {
    case "plugin":
      return route.pluginId === to.pluginId;
    case "agent":
      return route.section === to.section;
    case "session":
      return to.sessionId === undefined || route.sessionId === to.sessionId;
    default:
      return true;
  }
}
import {
  AGENT_SECTIONS_AREA,
  NAV_AREA,
  PLUGIN_PAGE_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
  type AgentSectionProps,
  type DirectiveChipData,
  type HeaderActionData,
  type NavRowData,
} from "./types";
import {
  FRAME_LOAD_TIMEOUT_MS,
  isCallbackHandle,
  isRpcVerb,
  type FrameToHost,
  type HostToFrame,
  type LoadedPlugin,
  type ShellSnapshot,
  type WireContribution,
} from "@/sandbox/protocol";

/* ---- the transport: a real frame, or whatever a test installs ---- */

export interface FrameTransport {
  post: (message: HostToFrame) => void;
  onMessage: (handler: (message: FrameToHost) => void) => void;
  /** the frame element, for the stage; absent in-process */
  element?: HTMLIFrameElement;
  destroy: () => void;
}

export type TransportFactory = (key: string) => FrameTransport;

function createIframeTransport(key: string): FrameTransport {
  const iframe = document.createElement("iframe");
  /* scripts only: no same-origin (so the origin is opaque and the shell's
     IPC refuses it), no forms, no popups, no top navigation */
  iframe.setAttribute("sandbox", "allow-scripts");
  iframe.setAttribute("title", `plugin ${key}`);
  iframe.dataset.uxFrame = key;
  stageElement().appendChild(iframe);
  /* parked until a viewport shows a surface: an unstyled frame is a
     300x150 box at the stage's corner, painting the runtime's background
     over the shell */
  placeFrame(key, iframe, null);
  const channel = new MessageChannel();
  let handler: ((message: FrameToHost) => void) | null = null;
  const queued: HostToFrame[] = [];
  let connected = false;
  let destroyed = false;
  let runtime = "";
  channel.port1.onmessage = (event: MessageEvent<FrameToHost>) => handler?.(event.data);
  iframe.addEventListener("load", () => {
    if (destroyed || !iframe.srcdoc) return;
    /* the frame's origin is opaque, so the only target that reaches it is "*";
       the port travels with the connect message and is the channel from
       here, and the runtime's text travels with it (frame-document.ts) */
    iframe.contentWindow?.postMessage({ t: "connect", runtime }, "*", [channel.port2]);
    connected = true;
    for (const message of queued.splice(0)) channel.port1.postMessage(message);
  });
  /* the document is written by the shell: the frame fetches nothing. A
     runtime that cannot be read surfaces as the frame's load failure, on
     the plugin's row. */
  void loadRuntimeAssets().then(
    (assets) => {
      if (destroyed) return;
      runtime = assets.js;
      iframe.srcdoc = sandboxDocument(frameNonce(), assets.css);
    },
    (error: unknown) => {
      if (destroyed) return;
      handler?.({
        t: "failed",
        error: error instanceof Error ? error.message : String(error),
      });
    },
  );
  return {
    element: iframe,
    post: (message) => {
      if (connected) channel.port1.postMessage(message);
      else queued.push(message);
    },
    onMessage: (next) => {
      handler = next;
    },
    destroy: () => {
      destroyed = true;
      channel.port1.close();
      iframe.remove();
    },
  };
}

let transportFactory: TransportFactory = createIframeTransport;

/** test seam: build frames in-process */
export function setSandboxTransportFactory(factory: TransportFactory | null): void {
  transportFactory = factory ?? createIframeTransport;
}

/* ---- the theme and the shell snapshot, pushed to every live frame ---- */

const liveHosts = new Set<SandboxHost>();

const currentTheme = (): "light" | "dark" =>
  typeof document !== "undefined" && document.documentElement.dataset.theme === "dark"
    ? "dark"
    : "light";

let themeObserver: MutationObserver | null = null;
function watchTheme() {
  if (
    themeObserver ||
    typeof MutationObserver === "undefined" ||
    typeof document === "undefined"
  )
    return;
  themeObserver = new MutationObserver(() => {
    const theme = currentTheme();
    for (const host of liveHosts) host.post({ t: "theme", theme });
  });
  themeObserver.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["data-theme"],
  });
}

function snapshot(host: UxHost): ShellSnapshot {
  return {
    route: host.shell.route(),
    deployments: host.shell.deployments() as unknown[],
    selectedDeployment: host.shell.selectedDeployment(),
    selectedSessionId: host.shell.selectedSessionId(),
  };
}

let shellSubscribed = false;
function watchShell() {
  if (shellSubscribed) return;
  const host = uxHost();
  if (!host?.onShellChange) return;
  shellSubscribed = true;
  host.onShellChange(() => {
    const shell = snapshot(host);
    for (const live of liveHosts) live.post({ t: "shell", shell });
  });
}

/* ---- the host ---- */

export interface SandboxHostOptions {
  /** the inventory key: the listing id, or the origin a loader names */
  key: string;
  /** the id the module must export; known from the listing or the door */
  pluginId: string;
  css?: string | null;
  listing?: string;
  declared?: UxDeclaredContributions;
}

/* storage as the shell keeps it, for the init message */
function readStorage(pluginId: string): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  const prefix = `gents.ux.${pluginId}.`;
  try {
    for (let i = 0; i < window.localStorage.length; i++) {
      const key = window.localStorage.key(i);
      if (!key?.startsWith(prefix)) continue;
      const raw = window.localStorage.getItem(key);
      if (raw === null) continue;
      try {
        out[key] = JSON.parse(raw);
      } catch {
        /* not ours to repair */
      }
    }
  } catch {
    /* no storage: nothing mirrored */
  }
  return out;
}

export class SandboxHost {
  private transport: FrameTransport | null = null;
  private ctx: UxContext | null = null;
  private readonly disposersByHandle = new Map<string, () => void>();
  private readonly anchors = new Map<string, HTMLElement>();
  private readonly offEvents: Array<() => void> = [];
  private loadedMeta: LoadedPlugin | null = null;
  private waiting: {
    loaded?: { resolve: (p: LoadedPlugin) => void; reject: (e: Error) => void };
    registered?: { resolve: () => void; reject: (e: Error) => void };
  } = {};

  constructor(
    private readonly source: string,
    readonly options: SandboxHostOptions,
  ) {}

  get live(): boolean {
    return this.transport !== null;
  }

  post(message: HostToFrame): void {
    this.transport?.post(message);
  }

  /** build the frame and evaluate the module; the plugin's own meta */
  load(): Promise<LoadedPlugin> {
    if (this.transport) this.dispose();
    const host = uxHost();
    if (!host)
      return Promise.reject(new Error("the UX plugin host is not installed yet"));
    watchTheme();
    watchShell();
    const transport = transportFactory(this.options.key);
    this.transport = transport;
    liveHosts.add(this);
    transport.onMessage((message) => this.receive(message));
    return new Promise<LoadedPlugin>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting.loaded = undefined;
        this.dispose();
        reject(
          new Error(
            `the plugin's frame did not load within ${FRAME_LOAD_TIMEOUT_MS / 1000}s`,
          ),
        );
      }, FRAME_LOAD_TIMEOUT_MS);
      this.waiting.loaded = {
        resolve: (meta) => {
          clearTimeout(timer);
          if (meta.id !== this.options.pluginId) {
            this.dispose();
            reject(
              new Error(
                `${this.options.key} exports a plugin with id "${meta.id}"; its listing declares "${this.options.pluginId}"`,
              ),
            );
            return;
          }
          this.loadedMeta = meta;
          resolve(meta);
        },
        reject: (error) => {
          clearTimeout(timer);
          this.dispose();
          reject(error);
        },
      };
      transport.post({
        t: "init",
        source: this.source,
        css: this.options.css,
        storage: readStorage(this.options.pluginId),
        shell: snapshot(host),
        theme: currentTheme(),
      });
    });
  }

  /** run register(ctx) in the frame; what it registers lands in `ctx` */
  async activate(ctx: UxContext): Promise<void> {
    if (!this.transport) await this.load();
    this.ctx = ctx;
    this.offEvents.push(
      onUxEvent("*", (event) =>
        this.post({ t: "event", type: event.type, payload: event.payload }),
      ),
    );
    await new Promise<void>((resolve, reject) => {
      this.waiting.registered = { resolve, reject };
      this.post({ t: "activate" });
    });
  }

  show(surface: string, props: Record<string, unknown>): void {
    this.post({ t: "show", surface, props });
  }

  hide(surface: string): void {
    this.post({ t: "hide", surface });
  }

  /** where the frame draws: the viewport element showing `surface`, or none */
  attach(surface: string, anchor: HTMLElement | null): void {
    if (anchor) this.anchors.set(surface, anchor);
    else this.anchors.delete(surface);
    const element = this.transport?.element;
    if (element)
      placeFrame(this.options.key, element, [...this.anchors.values()][0] ?? null);
  }

  dispose(): void {
    for (const off of this.offEvents.splice(0)) off();
    for (const dispose of this.disposersByHandle.values()) {
      try {
        dispose();
      } catch {
        /* a disposer with a bug must not block the rest */
      }
    }
    this.disposersByHandle.clear();
    this.anchors.clear();
    const transport = this.transport;
    this.transport = null;
    this.ctx = null;
    this.loadedMeta = null;
    liveHosts.delete(this);
    if (transport?.element) placeFrame(this.options.key, null, null);
    transport?.destroy();
    const waiting = this.waiting;
    this.waiting = {};
    waiting.loaded?.reject(new Error("the plugin was unloaded"));
    waiting.registered?.reject(new Error("the plugin was unloaded"));
  }

  get meta(): LoadedPlugin | null {
    return this.loadedMeta;
  }

  /* ---- messages from the frame ---- */

  private receive(message: FrameToHost): void {
    switch (message.t) {
      case "loaded": {
        const waiter = this.waiting.loaded;
        this.waiting.loaded = undefined;
        waiter?.resolve(message.plugin);
        return;
      }
      case "failed": {
        const waiter = this.waiting.loaded;
        this.waiting.loaded = undefined;
        waiter?.reject(new Error(message.error));
        return;
      }
      case "registered": {
        const waiter = this.waiting.registered;
        this.waiting.registered = undefined;
        if (message.ok) waiter?.resolve();
        else waiter?.reject(new Error(message.error));
        return;
      }
      case "register":
        this.registerAll(message.contributions);
        return;
      case "unregister":
        for (const handle of message.handles) {
          const dispose = this.disposersByHandle.get(handle);
          this.disposersByHandle.delete(handle);
          dispose?.();
        }
        return;
      case "call":
        void this.call(message.id, message.verb, message.args);
        return;
      case "render-error":
        console.error(
          `[ux-plugins] ${this.options.pluginId}: page ${message.surface} failed`,
          message.error,
        );
        return;
      case "log":
        console[message.level](
          `[ux-plugins] ${this.options.pluginId}: ${message.message}`,
        );
        return;
    }
  }

  /* a refused contribution (an undeclared area, a claimed directive name,
     a bad id) fails the plugin, as it did when register() threw in the
     shell: during the activation the round trip rejects; afterwards the
     row turns to error and the frame goes */
  private refuse(error: unknown): void {
    const waiter = this.waiting.registered;
    if (waiter) {
      this.waiting.registered = undefined;
      waiter.reject(error instanceof Error ? error : new Error(String(error)));
      return;
    }
    console.error(
      `[ux-plugins] ${this.options.pluginId}: refused after activation`,
      error,
    );
    patchUxPlugin(this.options.pluginId, {
      status: "error",
      error: error instanceof Error ? error.message : String(error),
    });
    unloadUxPlugin(this.options.pluginId);
  }

  private registerAll(contributions: WireContribution[]): void {
    const ctx = this.ctx;
    if (!ctx) return;
    for (const c of contributions) {
      try {
        const dispose = ctx.register(this.hostContribution(c));
        this.disposersByHandle.set(c.handle, dispose);
      } catch (error) {
        this.refuse(error);
        return;
      }
    }
  }

  /* a callback handle in shell-rendered data invokes the plugin's function
     in its frame; nothing of the plugin runs here */
  private invoke(handle: unknown, args: unknown[]): void {
    if (isCallbackHandle(handle))
      this.post({ t: "invoke", callback: handle.$cb, args });
  }

  private hostContribution(c: WireContribution): Parameters<UxContext["register"]>[0] {
    const base = {
      id: c.id,
      area: c.area,
      order: c.order,
      title: c.title,
      enabled: c.enabled,
    };
    const surface = `${this.options.pluginId}:${c.id}`;
    const data = (c.data ?? {}) as Record<string, unknown>;
    switch (c.area) {
      case PLUGIN_PAGE_AREA:
        return {
          ...base,
          render: () =>
            createElement(FrameViewport, {
              host: this,
              surface,
              props: {},
              fill: true,
            }),
        };
      case AGENT_SECTIONS_AREA:
        return {
          ...base,
          data: {
            group: data.group,
            label: data.label,
            icon: createElement(pluginIcon(data.icon), { className: "size-4" }),
            render: (props: AgentSectionProps) =>
              createElement(FrameViewport, {
                host: this,
                surface,
                props: props as unknown as Record<string, unknown>,
              }),
          },
        };
      case NAV_AREA: {
        const row = data as unknown as NavRowData;
        const to =
          typeof row.to === "object" && row.to !== null
            ? (row.to as Record<string, unknown>)
            : {};
        const item: NavItem = {
          id: typeof row.id === "string" ? row.id : c.id,
          label: typeof row.label === "string" ? row.label : c.id,
          icon: createElement(pluginIcon(row.icon), { className: "size-4" }),
          /* an agent section without a node means the selected node's;
             the row waits while there is none */
          to: (ctx) =>
            to.name === "agent" && typeof to.nodeDid !== "string"
              ? ctx.selectedNodeDid
                ? ({ ...to, nodeDid: ctx.selectedNodeDid } as Route)
                : null
              : (to as unknown as Route),
          active: (route) => routeMatches(to, route),
          placement: row.placement === "primary" ? "primary" : "footer",
          order: typeof row.order === "number" ? row.order : c.order,
        };
        return { ...base, data: item };
      }
      case SESSION_HEADER_ACTIONS_AREA: {
        const action = data as unknown as HeaderActionData;
        return {
          ...base,
          render: () =>
            createElement(HeaderActionButton, {
              label: String(action.label ?? c.id),
              title: action.title,
              icon: action.icon ?? null,
              onClick: () => this.invoke(action.onClick, []),
            }),
        };
      }
      case TRANSCRIPT_DIRECTIVE_AREA: {
        const chip = data as unknown as DirectiveChipData;
        return {
          ...base,
          data: {
            name: chip.name,
            render: (props: { attrs: Readonly<Record<string, string>> }) =>
              createElement(DirectiveChip, {
                label: String(chip.label ?? chip.name),
                icon: chip.icon ?? null,
                attrs: props.attrs,
                onClick: () => this.invoke(chip.onClick, [props.attrs]),
              }),
          },
        };
      }
      default:
        return { ...base, data: c.data };
    }
  }

  private async call(id: number, verb: string, args: unknown[]): Promise<void> {
    const reply = (value: unknown) => this.post({ t: "reply", id, ok: true, value });
    const refuse = (error: unknown) =>
      this.post({
        t: "reply",
        id,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      });
    const ctx = this.ctx;
    if (!ctx) return refuse(new Error("the plugin is not active"));
    if (!isRpcVerb(verb)) return refuse(new Error(`unknown verb "${verb}"`));
    try {
      switch (verb) {
        case "plugin":
          return reply(await ctx.plugin(String(args[0]), args[1]));
        case "send":
          await ctx.send(String(args[0]));
          return reply(undefined);
        case "navigate":
          ctx.shell.navigate(args[0] as Parameters<UxContext["shell"]["navigate"]>[0]);
          return reply(undefined);
        case "toast": {
          const kind = String(args[0]);
          const text = String(args[1] ?? "");
          if (kind === "success") toast.success(text);
          else if (kind === "error") toast.error(text);
          else if (kind === "info") toast.info(text);
          else if (kind === "warning") toast.warning(text);
          else toast(text);
          return reply(undefined);
        }
        case "storage.set":
          ctx.storage.set(String(args[0]), args[1]);
          return reply(undefined);
        case "storage.remove":
          ctx.storage.remove(String(args[0]));
          return reply(undefined);
        case "os.openExternal":
          return reply(await ctx.os.openExternal(String(args[0])));
        case "os.revealPath":
          return reply(await ctx.os.revealPath(String(args[0])));
        case "os.pickDirectory":
          return reply(
            await ctx.os.pickDirectory(
              (args[0] ?? undefined) as Parameters<UxContext["os"]["pickDirectory"]>[0],
            ),
          );
        case "data.call":
          return reply(
            await ctx.data.call(
              String(args[0]),
              String(args[1]),
              (args[2] ?? {}) as Record<string, unknown>,
            ),
          );
        case "data.surfaces":
          return reply(await ctx.data.surfaces());
      }
    } catch (error) {
      refuse(error);
    }
  }
}

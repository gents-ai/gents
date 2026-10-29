/* The wire between the shell and a plugin's sandbox frame: JSON messages
   over one MessagePort per plugin. The port is the plugin's identity on the
   host side (it was created for this listing and nothing else), so no
   message names a plugin. Functions never cross: a plugin's render
   functions stay in the frame and draw into it; a callback it hands a
   shell-rendered row (a click) crosses as a `{ $cb }` handle the host
   invokes back. */

/** what a plugin reads of the app, mirrored into the frame on every change */
export interface ShellSnapshot {
  route: unknown;
  deployments: unknown[];
  selectedDeployment: unknown | null;
  selectedSessionId: string | null;
}

/** a function the plugin registered in shell-rendered data (`onClick`) */
export interface CallbackHandle {
  $cb: string;
}

/** a contribution as it crosses: data is JSON with callback handles; a
    render function is reported as present and kept in the frame */
export interface WireContribution {
  handle: string;
  id: string;
  area: string;
  order?: number;
  title?: string;
  enabled?: boolean;
  data?: unknown;
  render?: boolean;
}

export interface LoadedPlugin {
  id: string;
  name?: string;
  description?: string;
  defaultEnabled?: boolean;
}

export type HostToFrame =
  | {
      t: "init";
      source: string;
      css?: string | null;
      storage: Record<string, unknown>;
      shell: ShellSnapshot;
      theme: "light" | "dark";
    }
  | { t: "activate" }
  | { t: "show"; surface: string; props: Record<string, unknown> }
  | { t: "hide"; surface: string }
  | { t: "event"; type: string; payload: unknown }
  | { t: "shell"; shell: ShellSnapshot }
  | { t: "theme"; theme: "light" | "dark" }
  | { t: "invoke"; callback: string; args: unknown[] }
  | { t: "reply"; id: number; ok: true; value: unknown }
  | { t: "reply"; id: number; ok: false; error: string };

export type FrameToHost =
  | { t: "loaded"; plugin: LoadedPlugin }
  | { t: "failed"; error: string }
  | { t: "register"; contributions: WireContribution[] }
  | { t: "unregister"; handles: string[] }
  | { t: "registered"; ok: true }
  | { t: "registered"; ok: false; error: string }
  | { t: "call"; id: number; verb: string; args: unknown[] }
  | { t: "render-error"; surface: string; error: string }
  | { t: "log"; level: "error" | "warn"; message: string };

/** the verbs a frame may call on the host; every one is scoped to the
    plugin the port belongs to, so the list is the plugin's whole reach */
export const RPC_VERBS = [
  "plugin",
  "send",
  "navigate",
  "toast",
  "storage.set",
  "storage.remove",
  "os.openExternal",
  "os.revealPath",
  "os.pickDirectory",
  "data.call",
  "data.surfaces",
] as const;
export type RpcVerb = (typeof RPC_VERBS)[number];

export const isRpcVerb = (verb: unknown): verb is RpcVerb =>
  typeof verb === "string" && (RPC_VERBS as readonly string[]).includes(verb);

/** the two areas whose contribution draws inside the frame */
export const FRAME_RENDERED_AREAS = ["plugin.pages", "agent.sections"] as const;

export const isCallbackHandle = (value: unknown): value is CallbackHandle =>
  typeof value === "object" &&
  value !== null &&
  typeof (value as CallbackHandle).$cb === "string";

/** A frame that never reports readiness must not leave its host waiting forever. */
export const FRAME_LOAD_TIMEOUT_MS = 8000;

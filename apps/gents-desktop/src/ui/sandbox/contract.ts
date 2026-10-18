/* The authoring contract, as a plugin sees it from inside its frame. A
   plugin is a module that default-exports a GentsUxPlugin and reaches the
   app only through the UxContext handed to `register`; every verb here is
   scoped to the plugin by the port the frame was built on, so what this
   interface lists is the plugin's whole reach. Shell-rendered contributions
   (a nav row, a header action, a directive chip) are data the shell draws;
   a page or a section is a render function that draws inside the frame. */
import type { ReactNode } from "react";
import type { Route } from "@/lib/router";
import type { Contribution } from "@/contrib/types";

/** a contribution as an author writes it: provenance and id scoping are the
    host's job, so those fields are off limits. `render` draws in the frame
    (`plugin.pages`, and `agent.sections` through its data); the other
    areas take data only. */
export type UxContribution = Omit<Contribution, "source" | "id" | "render" | "when"> & {
  id: string;
  render?: (props: Record<string, unknown>) => ReactNode;
};

/** namespaced JSON persistence under `gents.ux.<id>.*`, mirrored from the
    shell: reads are synchronous against the mirror, writes cross the port */
export interface UxStorage {
  get<T>(key: string, fallback: T): T;
  set(key: string, value: unknown): void;
  remove(key: string): void;
}

/** the OS door: every member resolves instead of throwing when the
    capability is absent, so a caller branches on the result */
export interface UxOs {
  openExternal: (url: string) => Promise<boolean>;
  revealPath: (path: string) => Promise<boolean>;
  pickDirectory: (options?: {
    title?: string;
    defaultPath?: string | null;
  }) => Promise<string | null>;
}

/** a deployment as the shell lists it; the fields a plugin may rely on */
export interface UxDeployment {
  nodeDid: string;
  label?: string;
  [key: string]: unknown;
}

/** what a plugin reads of the app (a snapshot the shell keeps current) and
    the two verbs it may call on it */
export interface UxShell {
  route: () => Route;
  navigate: (route: Route) => void;
  deployments: () => readonly UxDeployment[];
  selectedDeployment: () => UxDeployment | null;
  selectedSessionId: () => string | null;
  /** posts `content` as the user's next turn in the selected session, in
   *  the user's name; rejects without a selected session or outside a user
   *  gesture (a click or a key press in the page) */
  send: (content: string) => Promise<void>;
}

/** one call's answer from a data surface: a query's rows or a create's receipt */
export interface UxDataCallResult {
  kind: "query" | "create";
  output: unknown;
}

/** one tool of a surface, as the node declares it */
export interface UxDataToolDecl {
  kind: "query" | "create";
  tool_name: string;
  collection: string;
  description?: string;
  fields?: unknown;
  filter_fields?: unknown;
  [key: string]: unknown;
}

export interface UxDataSurfaceDecl {
  surface: string;
  enabled: boolean;
  tools: UxDataToolDecl[];
}

/** the data door: the surfaces the plugin's manifest names, on the
    selected node, as the node's own query/create declarations bound them.
    The shell fills the plugin and the node; the plugin names a surface, a
    tool and the tool's arguments. */
export interface UxData {
  /** run one declared tool: `args` are the tool's own (`fields`, `limit`,
      filter values, `field_page` for a long string; a create's field values) */
  call: (
    surface: string,
    tool: string,
    args?: Record<string, unknown>,
  ) => Promise<UxDataCallResult>;
  /** the declarations behind the plugin's surfaces, for typing a page */
  surfaces: () => Promise<UxDataSurfaceDecl[]>;
}

export interface UxContext {
  /** the resolved source tag, `'plugin:<id>'` */
  readonly source: string;
  readonly pluginId: string;
  register: (c: UxContribution) => () => void;
  registerMany: (cs: UxContribution[]) => () => void;
  /** a cleanup that is not a contribution (a store subscription) */
  onDispose: (fn: () => void) => void;
  /** the desktop update stream by type (`client-updated`); `'*'` for everything */
  onEvent: (
    type: string,
    listener: (event: { type: string; payload: unknown }) => void,
  ) => () => void;
  setTimeout: (fn: () => void, ms: number) => () => void;
  setInterval: (fn: () => void, ms: number) => () => void;
  addEventListener: (
    target: EventTarget,
    type: string,
    listener: EventListenerOrEventListenerObject,
    options?: AddEventListenerOptions | boolean,
  ) => () => void;
  /** one of this pack's .afb plugins by name; rejects for a plugin outside a pack */
  plugin: (name: string, input: unknown) => Promise<unknown>;
  /** see UxShell.send */
  send: (content: string) => Promise<void>;
  shell: UxShell;
  storage: UxStorage;
  os: UxOs;
  data: UxData;
}

export interface GentsUxPlugin {
  /** stable slug; the `plugin:<id>` source and the id namespace */
  id: string;
  name?: string;
  description?: string;
  /** registers on load when the user has not chosen; false ships opt-in */
  defaultEnabled?: boolean;
  register: (ctx: UxContext) => void | Promise<void>;
}

/** a plugin id: a snake_case name, the rule a pack and a dev folder follow
    (`is_valid_pack_name` in Rust). It prefixes storage keys, contribution
    ids and routes, so no `.`, `:`, `/` or whitespace. */
export const UX_PLUGIN_ID_RE = /^[a-z][a-z0-9_]{0,63}$/;

export function isValidUxPluginId(id: unknown): id is string {
  return typeof id === "string" && UX_PLUGIN_ID_RE.test(id);
}

/** what every loader checks before trusting a module's default export */
export function isGentsUxPlugin(value: unknown): value is GentsUxPlugin {
  return (
    typeof value === "object" &&
    value !== null &&
    isValidUxPluginId((value as { id?: unknown }).id) &&
    typeof (value as { register?: unknown }).register === "function"
  );
}

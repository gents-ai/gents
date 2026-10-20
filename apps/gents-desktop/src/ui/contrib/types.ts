/* The one primitive every extensible surface consumes. A bar renders these
   inline via <Slot>; a page picks one by id; an engine reads `data`. Copied
   from hermes-agent's apps/desktop/src/contrib/types.ts: the shape is the
   seam, so a plugin written against one app reads the same in the other. */
import type { ReactNode } from "react";

/** `'core'` is the app's own UI; a plugin's is `'plugin:<id>'`. The tag drives
    precedence now and the trust gate later. */
export type ContributionSource = "core" | (string & {});

export interface Contribution {
  /** stable within its area; re-registering an id replaces the earlier entry */
  id: string;
  /** which surface this lands on; see the AREA constants below */
  area: string;
  /** provenance; `'core'` when omitted */
  source?: ContributionSource;
  /** human label (a tab, a header) */
  title?: string;
  /** ascending within the area; ties keep registration order */
  order?: number;
  /** evaluated when the area's snapshot is rebuilt (a register/remove in
      that area), not reactively: a `when` that flips on outside state needs
      a registry mutation to be seen */
  when?: () => boolean;
  /** soft hide without unregistering */
  enabled?: boolean;
  /** Family A: renders the contribution (wrapped in ContribBoundary) */
  render?: () => ReactNode;
  /** Family B: a payload an engine reads (a nav row, a directive name) */
  data?: unknown;
}

/* The first-wave areas. Each names its payload type and its arbitration rule
   here, in one place, so a consumer and a plugin agree without a meeting. */

/** data: NavItem (ui/app/navRegistry.tsx). All mount in order; same id replaces. */
export const NAV_AREA = "nav";

/** data: AgentSectionData + render({ nodeDid, item }). All mount in order. */
export const AGENT_SECTIONS_AREA = "agent.sections";

/** render only. Trailing actions in a session's header. All mount in order. */
export const SESSION_HEADER_ACTIONS_AREA = "session.header.actions";

/** data: TranscriptDirectiveContribution. First registration of a name wins. */
export const TRANSCRIPT_DIRECTIVE_AREA = "transcript.directives";

/** render only. A plugin's own full page: what the plugin route
 *  (`{ name: "plugin", pluginId }`, the main nav's destination for a plugin's
 *  row) draws. One per plugin: the first the plugin registered here. */
export const PLUGIN_PAGE_AREA = "plugin.pages";

export const KNOWN_AREAS = [
  NAV_AREA,
  AGENT_SECTIONS_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
  PLUGIN_PAGE_AREA,
] as const;
export type KnownArea = (typeof KNOWN_AREAS)[number];

/** payload of an `agent.sections` contribution's `data`: a tab plus the page
    it renders for the route's node and item. A plugin names its icon (see
    ./icons.ts); the app's own sections pass a node. */
export interface AgentSectionData {
  /** the sidebar group the tab sits under ("Configure", "Packs", ...) */
  group: string;
  label: string;
  icon?: ReactNode | string | null;
  render: (props: AgentSectionProps) => ReactNode;
}

/** payload of a plugin's `nav` contribution: a row the rail and the nav
    panel draw. `to` is a route, or an agent section of the selected node
    (`{ name: "agent", section }` without a node); the row hides while
    there is no node to point at. `active` is derived from `to`. */
export interface NavRowData {
  id: string;
  label: string;
  icon?: string | null;
  to: unknown;
  placement?: "primary" | "footer";
  order?: number;
}

/** payload of a plugin's `session.header.actions` contribution: a button
    in the session's pane bar; the click reaches the plugin's frame */
export interface HeaderActionData {
  label: string;
  icon?: string | null;
  title?: string;
  onClick?: () => void;
}

/** payload of a plugin's `transcript.directives` contribution: a chip for
    `::name{...}` in the model's reply. `label` may name attributes as
    `{attr}`; the click gets the attributes. */
export interface DirectiveChipData {
  name: string;
  label: string;
  icon?: string | null;
  onClick?: (attrs: Readonly<Record<string, string>>) => void;
}

/** props an `agent.sections` page receives */
export interface AgentSectionProps {
  nodeDid: string;
  item?: string;
}

/** props handed to a directive contribution's `render` */
export interface TranscriptDirectiveProps {
  /** parsed, untrusted model output: a plugin validates its own fields */
  attrs: Readonly<Record<string, string>>;
  /** the original directive text, for diagnostics or a fallback */
  source: string;
  /** true while the surrounding message is still streaming */
  streaming: boolean;
}

/** payload of a `transcript.directives` contribution's `data` */
export interface TranscriptDirectiveContribution {
  /** what the model addresses: `::<name>{...}`; lowercase `[a-z0-9-]` */
  name: string;
  render: (props: TranscriptDirectiveProps) => ReactNode;
}

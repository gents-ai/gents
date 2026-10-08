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

/** data: AgentSectionData + render({ agentDid, item }). All mount in order. */
export const AGENT_SECTIONS_AREA = "agent.sections";

/** render only. Trailing actions in a session's header. All mount in order. */
export const SESSION_HEADER_ACTIONS_AREA = "session.header.actions";

/** data: TranscriptDirectiveContribution. First registration of a name wins. */
export const TRANSCRIPT_DIRECTIVE_AREA = "transcript.directives";

export const KNOWN_AREAS = [
  NAV_AREA,
  AGENT_SECTIONS_AREA,
  SESSION_HEADER_ACTIONS_AREA,
  TRANSCRIPT_DIRECTIVE_AREA,
] as const;
export type KnownArea = (typeof KNOWN_AREAS)[number];

/** payload of an `agent.sections` contribution's `data`: a tab plus the page
    it renders for the route's agent and item */
export interface AgentSectionData {
  /** the sidebar group the tab sits under ("Configure", "Packs", ...) */
  group: string;
  label: string;
  icon?: ReactNode;
  render: (props: AgentSectionProps) => ReactNode;
}

/** props an `agent.sections` page receives */
export interface AgentSectionProps {
  agentDid: string;
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

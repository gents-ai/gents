/* The agent configuration screen's sections, grouped by what the person is
   doing (gents-design config flows). Each is a route (`agent/<id>`), so a
   section is linkable, and a section in the sidebar names its group, label
   and mark. A route that has no entry of its own (an older link, or a page
   reached from another section) names the entry it lights up instead.
   Contexts have no entry: each behavior edits its own instructions and
   tools, and the Behaviors list links to unused contexts, so the `contexts`
   route stays for those links. */
import type { NodeView } from "../../../hooks/fleetStore";
import { useMemo, type ComponentType, type ReactNode } from "react";
import {
  Bot,
  FolderOpen,
  ListChecks,
  Package,
  Play,
  Plug,
  Puzzle,
  Radio,
  Sparkles,
  Timer,
  Workflow,
  Wrench,
  Zap,
} from "lucide-react";
import { ContribBoundary, ContribRender } from "@/contrib/react/boundary";
import { useContributions } from "@/contrib/react/use-contributions";
import { AGENT_SECTIONS_AREA, type AgentSectionData } from "@/contrib/types";
import { AgentPanel } from "./AgentPanel";
import { AllowedFoldersPanel } from "./AllowedFoldersPanel";
import { BehaviorsPanel } from "./BehaviorsPanel";
import { ContextsPanel } from "./ContextsPanel";
import { EventSourcesPanel } from "./EventSourcesPanel";
import { InferencePanel } from "./InferencePanel";
import { PacksPanel } from "./PacksPanel";
import { ProfilesPanel } from "./ProfilesPanel";
import { SchedulesPanel } from "./SchedulesPanel";
import { SkillsPanel } from "./SkillsPanel";
import { TasksPanel } from "./TasksPanel";
import { ToolServicesPanel } from "./ToolServicesPanel";
import { ToolsPanel } from "./ToolsPanel";
import { TriggersPanel } from "./TriggersPanel";
import { UxPluginsPanel } from "./UxPluginsPanel";

export type SectionPanelProps = {
  deployment: NodeView;
  /** the document the route opens, when it names one */
  item?: string;
};

type Listed = {
  /** the sidebar's group */
  group: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
  /** how many the sidebar shows beside the label */
  count?: (deployment: NodeView) => number;
};

export type AgentSection = {
  id: string;
  Panel: ComponentType<SectionPanelProps>;
} & (Listed | { under: string });

export const isListed = (section: AgentSection): section is AgentSection & Listed =>
  !("under" in section);

/** The sections, in sidebar order; groups show in the order their first section does. */
export const SECTIONS: AgentSection[] = [
  { group: "Configure", id: "agent", label: "Agent", icon: Bot, Panel: AgentPanel },
  {
    group: "Configure",
    id: "behaviors",
    label: "Behaviors",
    icon: Workflow,
    count: (d) => d.behaviors.length,
    Panel: ({ deployment, item }) => (
      <BehaviorsPanel deployment={deployment} behaviorId={item} />
    ),
  },
  { id: "contexts", under: "behaviors", Panel: ContextsPanel },
  {
    group: "Configure",
    id: "skills",
    label: "Skills",
    icon: ListChecks,
    count: (d) => d.skills.length,
    Panel: SkillsPanel,
  },
  {
    group: "Configure",
    id: "profiles",
    label: "Providers",
    icon: Sparkles,
    count: (d) => d.inferenceBackends.length,
    Panel: ProfilesPanel,
  },
  /* a backend opens on its own; the list is the Providers page, with each
     backend's profiles under it */
  {
    id: "inference",
    under: "profiles",
    Panel: ({ deployment, item }) =>
      item ? (
        <InferencePanel deployment={deployment} item={item} />
      ) : (
        <ProfilesPanel deployment={deployment} />
      ),
  },
  {
    group: "Automation",
    id: "tasks",
    label: "Tasks",
    icon: Play,
    count: (d) => d.tasks.length,
    Panel: TasksPanel,
  },
  {
    group: "Automation",
    id: "triggers",
    label: "Triggers",
    icon: Zap,
    count: (d) => d.triggers.length,
    Panel: TriggersPanel,
  },
  { id: "automations", under: "triggers", Panel: TriggersPanel },
  {
    group: "Automation",
    id: "schedules",
    label: "Schedules",
    icon: Timer,
    count: (d) => d.schedules.length,
    Panel: SchedulesPanel,
  },
  {
    group: "Automation",
    id: "event-sources",
    label: "Event sources",
    icon: Radio,
    count: (d) => d.eventSources.length,
    Panel: EventSourcesPanel,
  },
  {
    group: "Tools",
    id: "tools",
    label: "Tools",
    icon: Wrench,
    count: (d) => d.tools.length,
    Panel: ToolsPanel,
  },
  {
    group: "Tools",
    id: "tool-services",
    label: "Remote Tools",
    icon: Plug,
    count: (d) => d.toolServiceRegistries.length,
    Panel: ToolServicesPanel,
  },
  {
    group: "Tools",
    id: "folders",
    label: "Allowed folders",
    icon: FolderOpen,
    Panel: AllowedFoldersPanel,
  },
  { group: "Packs", id: "packs", label: "Packs", icon: Package, Panel: PacksPanel },
  {
    group: "Packs",
    id: "ux-plugins",
    label: "UX Plugins",
    icon: Puzzle,
    Panel: UxPluginsPanel,
  },
];

/* A section a UX plugin contributes through the `agent.sections` area: a
   listed tab in the group it names, drawing the page it carries. Its route
   id is the registry's namespaced contribution id (`<plugin>:<local>`),
   already unique and already a legal route segment. The page is mounted
   behind a boundary, so a plugin's throw stays on its own tab. */
function isSectionData(data: unknown): data is AgentSectionData {
  return (
    typeof data === "object" &&
    data !== null &&
    typeof (data as AgentSectionData).group === "string" &&
    typeof (data as AgentSectionData).label === "string" &&
    typeof (data as AgentSectionData).render === "function"
  );
}

/* a plugin's icon is a node, not a component; the sidebar wants a component */
const iconOf = (node: ReactNode): ComponentType<{ className?: string }> =>
  node === undefined || node === null ? Puzzle : () => <>{node}</>;

function contributedSection(id: string, data: AgentSectionData): AgentSection {
  return {
    group: data.group,
    id,
    label: data.label,
    icon: iconOf(data.icon),
    Panel: ({ deployment, item }) => (
      <ContribBoundary id={id} variant="pane">
        {/* mounted as a component, so a throw in the page's render is the
            boundary's to catch and its hooks are its own */}
        <ContribRender
          render={() => data.render({ agentDid: deployment.agentDid, item })}
        />
      </ContribBoundary>
    ),
  };
}

/** The app's sections and every contributed one, in sidebar order;
    re-renders when a plugin registers or unregisters a section. */
export function useAgentSections(): readonly AgentSection[] {
  const contributed = useContributions(AGENT_SECTIONS_AREA);
  return useMemo(
    () => [
      ...SECTIONS,
      ...contributed.flatMap((c) =>
        isSectionData(c.data) ? [contributedSection(c.id, c.data)] : [],
      ),
    ],
    [contributed],
  );
}

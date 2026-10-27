import type { NodeView } from "../../../hooks/fleetStore";
import { useMemo, type ComponentType, type ReactNode } from "react";
import {
  FolderOpen,
  ListChecks,
  Package,
  Play,
  Plug,
  Puzzle,
  Radio,
  Route,
  Sparkles,
  Timer,
  Waypoints,
  Workflow,
  Wrench,
  Zap,
} from "lucide-react";
import { ContribBoundary, ContribRender } from "@/contrib/react/boundary";
import { useContributions } from "@/contrib/react/use-contributions";
import { AGENT_SECTIONS_AREA, type AgentSectionData } from "@/contrib/types";
import { AgentPanel } from "./AgentPanel";
import { AllowedFoldersPanel } from "./AllowedFoldersPanel";
import { AgentsPanel } from "./BehaviorsPanel";
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
import { WorkflowsPanel } from "./WorkflowsPanel";

export type SectionPanelProps = {
  deployment: NodeView;
  item?: string;
};

type Listed = {
  group: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
  count?: (deployment: NodeView) => number;
};

export type AgentSection = {
  id: string;
  Panel: ComponentType<SectionPanelProps>;
} & (Listed | { under: string });

export const isListed = (section: AgentSection): section is AgentSection & Listed =>
  !("under" in section);

export const SECTIONS: AgentSection[] = [
  {
    group: "Configure",
    id: "agent",
    label: "Node",
    icon: Waypoints,
    Panel: AgentPanel,
  },
  {
    group: "Configure",
    id: "agents",
    label: "Agents",
    icon: Workflow,
    count: (d) => d.agents.length,
    Panel: ({ deployment, item }) => (
      <AgentsPanel deployment={deployment} agentId={item} />
    ),
  },
  { id: "contexts", under: "agents", Panel: ContextsPanel },
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
    group: "Automation",
    id: "workflows",
    label: "Workflows",
    icon: Route,
    Panel: WorkflowsPanel,
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

/* the section id becomes a route segment; one that could not round-trip
   through the route is left out rather than left unreachable */
const isRouteSegment = (id: string) => !/[\s/?#]/.test(id);

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
        {/* the plugin's own render function, mounted once per registration:
            a throw is the boundary's to catch, its hooks are its own, and the
            screen's re-renders reach it as new props */}
        <ContribRender
          render={data.render}
          props={{ nodeDid: deployment.nodeDid, item }}
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
        isSectionData(c.data) && isRouteSegment(c.id)
          ? [contributedSection(c.id, c.data)]
          : [],
      ),
    ],
    [contributed],
  );
}

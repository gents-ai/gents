import type { NodeView } from "../../../hooks/fleetStore";
import type { ComponentType } from "react";
import {
  Bot,
  FolderOpen,
  ListChecks,
  Package,
  Play,
  Plug,
  Radio,
  Sparkles,
  Timer,
  Workflow,
  Wrench,
  Zap,
} from "lucide-react";
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
  { group: "Configure", id: "agent", label: "Agent", icon: Bot, Panel: AgentPanel },
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
];

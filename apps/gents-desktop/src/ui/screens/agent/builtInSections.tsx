/* The configuration sections the app brings, grouped by what the person is
   doing (gents-design config flows). Importing this module once fills the
   section registry. Contexts have no entry: each behavior edits its own
   instructions and tools, and the Behaviors list links to unused contexts,
   so the `contexts` route stays for those links. */
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
import { BehaviorsPanel } from "./BehaviorsPanel";
import { ContextsPanel } from "./ContextsPanel";
import { EventSourcesPanel } from "./EventSourcesPanel";
import { InferencePanel } from "./InferencePanel";
import { PacksPanel } from "./PacksPanel";
import { ProfilesPanel } from "./ProfilesPanel";
import { SchedulesPanel } from "./SchedulesPanel";
import { agentSections, type AgentSection } from "./sections";
import { SkillsPanel } from "./SkillsPanel";
import { TasksPanel } from "./TasksPanel";
import { ToolServicesPanel } from "./ToolServicesPanel";
import { ToolsPanel } from "./ToolsPanel";
import { TriggersPanel } from "./TriggersPanel";

const BUILT_IN: AgentSection[] = [
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
];

for (const section of BUILT_IN) agentSections.register(section);

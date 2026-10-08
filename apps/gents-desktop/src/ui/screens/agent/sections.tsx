import { useMemo, type ReactNode } from "react";
import {
  Bot,
  FolderOpen,
  Plug,
  ListChecks,
  Package,
  Play,
  Puzzle,
  Radio,
  Sparkles,
  Timer,
  Workflow,
  Wrench,
  Zap,
} from "lucide-react";
import { useContributions } from "@/contrib/react/use-contributions";
import {
  AGENT_SECTIONS_AREA,
  type AgentSectionData,
  type AgentSectionProps,
} from "@/contrib/types";

/* the config tabs, grouped by what the user is doing (gents-design config
   flows). Contexts have no entry: each behavior edits its own instructions
   and tools, and the Behaviors list links to unused contexts, so the
   `contexts` route stays for those links. */
export const SECTIONS = [
  { group: "Configure", id: "agent", label: "Agent", icon: Bot },
  { group: "Configure", id: "behaviors", label: "Behaviors", icon: Workflow },
  { group: "Configure", id: "skills", label: "Skills", icon: ListChecks },
  { group: "Configure", id: "profiles", label: "Providers", icon: Sparkles },
  { group: "Automation", id: "tasks", label: "Tasks", icon: Play },
  { group: "Automation", id: "triggers", label: "Triggers", icon: Zap },
  { group: "Automation", id: "schedules", label: "Schedules", icon: Timer },
  { group: "Automation", id: "event-sources", label: "Event sources", icon: Radio },
  { group: "Tools", id: "tools", label: "Tools", icon: Wrench },
  { group: "Tools", id: "tool-services", label: "Remote Tools", icon: Plug },
  { group: "Tools", id: "folders", label: "Allowed folders", icon: FolderOpen },
  { group: "Packs", id: "packs", label: "Packs", icon: Package },
  { group: "Packs", id: "ux-plugins", label: "UX Plugins", icon: Puzzle },
] as const;
export type SectionId = (typeof SECTIONS)[number]["id"];

/* one row of the section nav, whether the app's own or a plugin's: the
   core list and the `agent.sections` area merge at render (the Hermes
   "core static list + contributed rows" shape). A contributed section
   carries the page it renders; a core one is rendered by AgentScreen's own
   switch. */
export interface AgentSection {
  id: string;
  group: string;
  label: string;
  icon: ReactNode;
  /** present for a contributed section; absent for a core one */
  renderSection?: (props: AgentSectionProps) => ReactNode;
}

const CORE_SECTIONS: readonly AgentSection[] = SECTIONS.map((s) => ({
  id: s.id,
  group: s.group,
  label: s.label,
  icon: <s.icon className="size-4" />,
}));

function isSectionData(data: unknown): data is AgentSectionData {
  return (
    typeof data === "object" &&
    data !== null &&
    typeof (data as AgentSectionData).group === "string" &&
    typeof (data as AgentSectionData).label === "string" &&
    typeof (data as AgentSectionData).render === "function"
  );
}

/* the route's section id for a contribution is the registry's namespaced
   id (`<plugin>:<local>`): already unique, already a legal route segment */
export function useAgentSections(): readonly AgentSection[] {
  const contributed = useContributions(AGENT_SECTIONS_AREA);
  return useMemo(() => {
    const rows: AgentSection[] = [...CORE_SECTIONS];
    for (const c of contributed) {
      if (!isSectionData(c.data)) continue;
      rows.push({
        id: c.id,
        group: c.data.group,
        label: c.data.label,
        icon: c.data.icon ?? <Puzzle className="size-4" />,
        renderSection: c.data.render,
      });
    }
    return rows;
  }, [contributed]);
}

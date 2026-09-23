import {
  Bot,
  Plug,
  ListChecks,
  Play,
  Radio,
  Sparkles,
  Timer,
  Workflow,
  Wrench,
  Zap,
} from "lucide-react";

/* the desktop app's config tabs, grouped by what the user is doing (see
   CONFIG-FLOWS.md). PROPOSED: contexts have no entry, and triggers are
   Automations; the `triggers` route still renders them. Each behavior edits its own
   instructions and tools, and the Behaviors list links to unused contexts;
   the `contexts` route stays for those links. */
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
] as const;
export type SectionId = (typeof SECTIONS)[number]["id"];

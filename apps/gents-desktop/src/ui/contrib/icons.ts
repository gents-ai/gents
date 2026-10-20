/* The icons a plugin may name in a shell-rendered contribution (a nav row,
   a header action, a chip, a section tab). A name, not a component: a
   component would be plugin code drawn in the shell. Unknown names draw
   the puzzle piece. */
import {
  Book,
  Bot,
  Calendar,
  Check,
  Clock,
  FileText,
  Folder,
  KanbanSquare,
  ListChecks,
  MessageSquare,
  Puzzle,
  Search,
  Settings,
  Sparkles,
  Star,
  Timer,
  Workflow,
  Zap,
  type LucideIcon,
} from "lucide-react";

export const PLUGIN_ICONS: Readonly<Record<string, LucideIcon>> = {
  book: Book,
  bot: Bot,
  calendar: Calendar,
  check: Check,
  clock: Clock,
  file: FileText,
  folder: Folder,
  kanban: KanbanSquare,
  list: ListChecks,
  message: MessageSquare,
  puzzle: Puzzle,
  search: Search,
  settings: Settings,
  sparkles: Sparkles,
  star: Star,
  timer: Timer,
  workflow: Workflow,
  zap: Zap,
};

export function pluginIcon(name: unknown): LucideIcon {
  return (typeof name === "string" && PLUGIN_ICONS[name]) || Puzzle;
}

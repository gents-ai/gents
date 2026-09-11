import {
  Bot,
  Brain,
  Cpu,
  ListChecks,
  Play,
  SlidersHorizontal,
  Sparkles,
  Timer,
  Wrench,
  Zap,
} from 'lucide-react'

/* the desktop app's config tabs, grouped the way the Settings design groups its sidebar */
export const SECTIONS = [
  { group: 'Configure', id: 'agent', label: 'Agent', icon: Bot },
  { group: 'Configure', id: 'behaviors', label: 'Behaviours', icon: Sparkles },
  { group: 'Configure', id: 'contexts', label: 'Contexts', icon: Sparkles },
  { group: 'Configure', id: 'skills', label: 'Skills', icon: ListChecks },
  { group: 'Inference', id: 'inference', label: 'Backends', icon: Cpu },
  { group: 'Inference', id: 'profiles', label: 'Profiles', icon: SlidersHorizontal },
  { group: 'Tools', id: 'tools', label: 'Tools', icon: Wrench },
  { group: 'Tools', id: 'tool-services', label: 'Tool services', icon: Brain },
  { group: 'Automation', id: 'tasks', label: 'Tasks', icon: Play },
  { group: 'Automation', id: 'schedules', label: 'Schedules', icon: Timer },
  { group: 'Automation', id: 'event-sources', label: 'Event sources', icon: Zap },
  { group: 'Automation', id: 'triggers', label: 'Triggers', icon: Zap },
] as const
export type SectionId = (typeof SECTIONS)[number]['id']

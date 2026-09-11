/* Agent configuration: the Settings screen from the Branding file. A
   sidebar of groups (the desktop app's config tabs, grouped) beside a
   page of field rows. Sections are routes, so a tab is linkable. */
import { SidebarGroup, SidebarItem, SidebarNav } from '@gents/ui/components/sidebar-nav'
import { ScrollArea } from '@gents/ui/components/scroll-area'
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from '@gents/ui/components/select'
import type { Shell } from '@/hooks/useShell'
import { href, navigate } from '@/lib/router'
import { AgentPanel } from './AgentPanel'
import { BehaviorsPanel } from './BehaviorsPanel'
import { ProfilesPanel } from './ProfilesPanel'
import { InferencePanel } from './InferencePanel'
import { SchedulesPanel } from './SchedulesPanel'
import { SkillsPanel } from './SkillsPanel'
import { TasksPanel } from './TasksPanel'
import { ContextsPanel } from './ContextsPanel'
import { EventSourcesPanel } from './EventSourcesPanel'
import { ToolsPanel } from './ToolsPanel'
import { ToolServicesPanel } from './ToolServicesPanel'
import { TriggersPanel } from './TriggersPanel'
import { SECTIONS, type SectionId } from './sections'

export function AgentScreen({
  shell,
  agentDid,
  section,
  item,
}: {
  shell: Shell
  agentDid: string
  section: string
  item?: string
}) {
  const deployment = shell.deployments.find((d) => d.agentDid === agentDid) ?? null
  if (!deployment) {
    return (
      <p className="p-8 text-sm text-muted-foreground">No agent with that DID on this desktop.</p>
    )
  }
  const counts: Partial<Record<SectionId, number>> = {
    behaviors: deployment.behaviors.length,
    contexts: deployment.contexts.length,
    skills: deployment.skills.length,
    inference: deployment.inferenceBackends.length,
    profiles: deployment.inferenceProfiles.length,
    tools: deployment.tools.length,
    'tool-services': deployment.toolServiceRegistries.length,
    tasks: deployment.tasks.length,
    schedules: deployment.schedules.length,
    'event-sources': deployment.eventSources.length,
    triggers: deployment.triggers.length,
  }
  const groups = [...new Set(SECTIONS.map((s) => s.group))]
  return (
    <div className="grid h-full min-h-0 grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] md:grid-cols-[20rem_minmax(0,1fr)]">
      <ScrollArea className="hidden h-full md:block">
        <SidebarNav className="w-auto">
          {groups.map((group) => (
            <SidebarGroup key={group} title={group}>
              {SECTIONS.filter((s) => s.group === group).map((s) => (
                <SidebarItem
                  key={s.id}
                  href={href({ name: 'agent', agentDid, section: s.id })}
                  icon={<s.icon />}
                  active={section === s.id}
                  count={counts[s.id]}
                >
                  {s.label}
                </SidebarItem>
              ))}
            </SidebarGroup>
          ))}
        </SidebarNav>
      </ScrollArea>
      <ScrollArea className="h-full">
        {/* below md the sidebar becomes a sticky section picker */}
        <div className="sticky top-0 z-10 border-b border-border/60 bg-background/95 px-4 py-2 backdrop-blur md:hidden">
          <Select
            items={SECTIONS.map((s) => ({ value: s.id, label: s.label }))}
            value={section}
            onValueChange={(v) => v && navigate({ name: 'agent', agentDid, section: v })}
          >
            <SelectTrigger aria-label="Section" className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {groups.map((group) => (
                <SelectGroup key={group}>
                  <SelectLabel>{group}</SelectLabel>
                  {SECTIONS.filter((s) => s.group === group).map((s) => (
                    <SelectItem key={s.id} value={s.id}>
                      <span className="flex items-center gap-2">
                        <s.icon className="size-4 text-muted-foreground" />
                        {s.label}
                        {counts[s.id] ? (
                          <span className="ml-auto pl-3 text-xs text-muted-foreground">
                            {counts[s.id]}
                          </span>
                        ) : null}
                      </span>
                    </SelectItem>
                  ))}
                </SelectGroup>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="mx-auto max-w-page px-4 py-6 md:px-8 md:py-8">
          {section === 'agent' && (
            <AgentPanel
              key={JSON.stringify(deployment.agentPrincipal)}
              shell={shell}
              deployment={deployment}
            />
          )}
          {section === 'behaviors' && (
            <BehaviorsPanel shell={shell} deployment={deployment} behaviorId={item} />
          )}
          {section === 'contexts' && (
            <ContextsPanel shell={shell} deployment={deployment} item={item} />
          )}
          {section === 'skills' && (
            <SkillsPanel shell={shell} deployment={deployment} item={item} />
          )}
          {section === 'inference' && (
            <InferencePanel shell={shell} deployment={deployment} item={item} />
          )}
          {section === 'profiles' && (
            <ProfilesPanel shell={shell} deployment={deployment} item={item} />
          )}
          {section === 'tools' && <ToolsPanel shell={shell} deployment={deployment} item={item} />}
          {section === 'tool-services' && (
            <ToolServicesPanel shell={shell} deployment={deployment} item={item} />
          )}
          {section === 'tasks' && <TasksPanel shell={shell} deployment={deployment} item={item} />}
          {section === 'schedules' && (
            <SchedulesPanel shell={shell} deployment={deployment} item={item} />
          )}
          {section === 'event-sources' && (
            <EventSourcesPanel shell={shell} deployment={deployment} item={item} />
          )}
          {section === 'triggers' && (
            <TriggersPanel shell={shell} deployment={deployment} item={item} />
          )}
        </div>
      </ScrollArea>
    </div>
  )
}

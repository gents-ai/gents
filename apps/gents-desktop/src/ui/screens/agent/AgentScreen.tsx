/* Agent configuration: a sidebar of grouped sections beside a page of
   field rows. Sections are routes, so a section is linkable. */
import { useEffect, useRef } from "react";
import {
  SidebarGroup,
  SidebarItem,
  SidebarNav,
} from "@gents/ui/components/sidebar-nav";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { href, navigate } from "@/lib/router";
import { useFleet } from "@/hooks/useFleet";
import { nodeOf } from "../../../hooks/fleetStore";
import { isListed, SECTIONS } from "./sections";

export function AgentScreen({
  nodeDid,
  section,
  item,
}: {
  nodeDid: string;
  section: string;
  item?: string;
}) {
  /* a new section, document or agent starts at the top; the scroll area
     keeps its position across hash changes otherwise */
  const content = useRef<HTMLDivElement>(null);
  useEffect(() => {
    content.current
      ?.querySelector("[data-slot=scroll-area-viewport]")
      ?.scrollTo({ top: 0 });
  }, [nodeDid, section, item]);
  const deployment = useFleet((s) => nodeOf(s, nodeDid));
  if (!deployment) {
    return (
      <p className="p-8 text-sm text-muted-foreground">
        No node with that DID on this desktop.
      </p>
    );
  }
  const current = SECTIONS.find((s) => s.id === section) ?? null;
  /* the sidebar entry this route lights up: its own, or the one it sits under */
  const entry = current && !isListed(current) ? current.under : section;
  const listed = SECTIONS.filter(isListed);
  const groups = [...new Set(listed.map((s) => s.group))];
  const Panel = current?.Panel;
  return (
    /* the sidebar's size container is this screen's own: WebKit restyles
       everything inside a size container at each width it passes through,
       which over the whole pane would include a long transcript */
    <div className="@container h-full min-h-0">
      <div
        className="grid h-full min-h-0 grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] @4xl:grid-cols-[20rem_minmax(0,1fr)]"
        data-testid="agent-screen"
        data-section={section}
      >
        <ScrollArea className="hidden h-full @4xl:block">
          <SidebarNav className="w-auto">
            {groups.map((group) => (
              <SidebarGroup key={group} title={group}>
                {listed
                  .filter((s) => s.group === group)
                  .map((s) => (
                    <SidebarItem
                      key={s.id}
                      href={href({ name: "agent", nodeDid, section: s.id })}
                      icon={<s.icon />}
                      active={entry === s.id}
                      count={s.count?.(deployment)}
                    >
                      {s.label}
                    </SidebarItem>
                  ))}
              </SidebarGroup>
            ))}
          </SidebarNav>
        </ScrollArea>
        <ScrollArea ref={content} className="h-full">
          {/* the section list needs room beside the editor, whatever the nav
              takes; without it the list becomes a sticky section picker */}
          <div className="sticky top-0 z-10 border-b border-border/60 bg-background/95 px-4 py-2 backdrop-blur @4xl:hidden">
            <Select
              items={listed.map((s) => ({ value: s.id, label: s.label }))}
              value={entry}
              onValueChange={(v) =>
                v && navigate({ name: "agent", nodeDid, section: v })
              }
            >
              <SelectTrigger aria-label="Section" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {groups.map((group) => (
                  <SelectGroup key={group}>
                    <SelectLabel>{group}</SelectLabel>
                    {listed
                      .filter((s) => s.group === group)
                      .map((s) => (
                        <SelectItem key={s.id} value={s.id}>
                          <span className="flex items-center gap-2">
                            <s.icon className="size-4 text-muted-foreground" />
                            {s.label}
                            {s.count?.(deployment) ? (
                              <span className="ml-auto pl-3 text-xs text-muted-foreground">
                                {s.count(deployment)}
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
          {/* a panel's drafts and dialogs belong to the agent they were opened
              on: switching agents starts every panel over */}
          <div
            key={deployment.nodeDid}
            className="mx-auto max-w-page px-4 py-6 md:px-8 md:py-8"
          >
            {Panel && <Panel deployment={deployment} item={item} />}
          </div>
        </ScrollArea>
      </div>
    </div>
  );
}

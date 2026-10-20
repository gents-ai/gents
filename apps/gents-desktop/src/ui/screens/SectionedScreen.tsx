import { useEffect, useRef, type ComponentType, type ReactNode } from "react";
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

/** one entry of a screen's section sidebar */
export type SectionEntry = {
  id: string;
  group: string;
  label: string;
  icon: ComponentType<{ className?: string }>;
  count?: number;
};

/**
 * A screen split into sections: a sidebar of grouped entries on a wide
 * container, a select above the page on a narrow one, and the page column
 * at the app's page width. The page scrolls back to its top when the
 * section, the item or the subject changes.
 */
export function SectionedScreen({
  entries,
  current,
  hrefFor,
  onSelect,
  scrollKey,
  pageKey,
  testId,
  children,
}: {
  entries: SectionEntry[];
  /** the listed entry the current section falls under */
  current: string;
  hrefFor: (id: string) => string;
  onSelect: (id: string) => void;
  /** a change of this puts the page back at its top */
  scrollKey: string;
  /** a change of this draws the page anew */
  pageKey?: string;
  testId: string;
  children: ReactNode;
}) {
  const content = useRef<HTMLDivElement>(null);
  useEffect(() => {
    content.current
      ?.querySelector("[data-slot=scroll-area-viewport]")
      ?.scrollTo({ top: 0 });
  }, [scrollKey]);
  const groups = [...new Set(entries.map((s) => s.group))];
  return (
    <div className="@container h-full min-h-0">
      <div
        className="grid h-full min-h-0 grid-cols-[minmax(0,1fr)] grid-rows-[minmax(0,1fr)] @4xl:grid-cols-[20rem_minmax(0,1fr)]"
        data-testid={testId}
        data-section={current}
      >
        <ScrollArea className="hidden h-full @4xl:block">
          <SidebarNav className="w-auto">
            {groups.map((group) => (
              <SidebarGroup key={group} title={group}>
                {entries
                  .filter((s) => s.group === group)
                  .map((s) => (
                    <SidebarItem
                      key={s.id}
                      href={hrefFor(s.id)}
                      icon={<s.icon />}
                      active={current === s.id}
                      count={s.count}
                    >
                      {s.label}
                    </SidebarItem>
                  ))}
              </SidebarGroup>
            ))}
          </SidebarNav>
        </ScrollArea>
        <ScrollArea ref={content} className="h-full">
          <div className="sticky top-0 z-10 border-b border-border/60 bg-background/95 px-4 py-2 backdrop-blur @4xl:hidden">
            <Select
              items={entries.map((s) => ({ value: s.id, label: s.label }))}
              value={current}
              onValueChange={(v) => v && onSelect(v)}
            >
              <SelectTrigger aria-label="Section" className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {groups.map((group) => (
                  <SelectGroup key={group}>
                    <SelectLabel>{group}</SelectLabel>
                    {entries
                      .filter((s) => s.group === group)
                      .map((s) => (
                        <SelectItem key={s.id} value={s.id}>
                          <span className="flex items-center gap-2">
                            <s.icon className="size-4 text-muted-foreground" />
                            {s.label}
                            {s.count ? (
                              <span className="ml-auto pl-3 text-xs text-muted-foreground">
                                {s.count}
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
          <div key={pageKey} className="mx-auto max-w-page px-4 py-6 md:px-8 md:py-8">
            {children}
          </div>
        </ScrollArea>
      </div>
    </div>
  );
}

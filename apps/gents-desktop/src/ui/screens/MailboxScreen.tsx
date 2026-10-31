import { Inbox, Search, Plus, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Tabs, TabsList, TabsTrigger } from "@gents/ui/components/tabs";
import { cn } from "@gents/ui/lib/utils";
import { href } from "@/lib/router";
import { useSelectedNodeDid } from "@/hooks/useClient";
import { listViews, useListViews, type MailboxTab } from "@/app/listViews";
import { TRIAGE } from "./mailbox-triage";
import { ItemRow } from "./MailboxItem";
import { MAILBOX, openHere, useMailboxList } from "./MailboxStack";
import { useMinute } from "./time";

type Tab = MailboxTab;

export function MailboxScreen() {
  const selectedNodeDid = useSelectedNodeDid();
  /* ages and due times move on while the screen stays open */
  const now = useMinute();
  /* the search, the tab and the open item live in the list views, which the
     app-wide dialog (MailboxStack) reads too */
  const query = useListViews((views) => views.mailboxQuery);
  const setQuery = listViews.setMailboxQuery;
  const tab = useListViews((views) => views.mailboxTab);
  const kept = useListViews((views) => views.mailboxOpen);
  const openId = openHere(kept, selectedNodeDid, MAILBOX);
  const { items, searched, groups, shown } = useMailboxList(query ?? "");
  const tabs = TRIAGE.map((t) => ({
    key: t.key as Tab,
    label: t.label,
    count: groups.find((g) => g.key === t.key)?.items.length ?? 0,
  }));

  return (
    <ScrollArea className="h-full" data-testid="mailbox-screen">
      <div className="mx-auto max-w-page px-6 py-6">
        {items.length > 0 && (
          <>
            <div className="flex items-center gap-2">
              <div
                className="min-w-0 flex-1 overflow-x-auto py-1"
                data-testid="mailbox-tabs-scroll"
              >
                <Tabs
                  value={tab}
                  onValueChange={(value) => listViews.setMailboxTab(value as Tab)}
                  className="w-max"
                >
                  <TabsList
                    variant="line"
                    aria-label="Mailbox groups"
                    className="min-h-11"
                  >
                    <TabsTrigger value="all" className={TAB}>
                      All <Count n={searched.length} />
                    </TabsTrigger>
                    {tabs.map((t) => (
                      <TabsTrigger key={t.key} value={t.key} className={TAB}>
                        {t.label} <Count n={t.count} />
                      </TabsTrigger>
                    ))}
                  </TabsList>
                </Tabs>
              </div>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Search the mailbox"
                aria-expanded={query !== null}
                className={cn(
                  "ml-auto shrink-0 text-muted-foreground",
                  query !== null && "text-foreground",
                )}
                onClick={() => setQuery(query === null ? "" : null)}
              >
                <Search />
              </Button>
            </div>
            {query !== null && (
              <div className="relative mt-3">
                <Input
                  autoFocus
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={(e) => e.key === "Escape" && setQuery(null)}
                  placeholder="Search titles, summaries, payloads"
                  className="h-9 w-full pr-9"
                  aria-label="Search the mailbox"
                />
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Clear search"
                  className="absolute top-1/2 right-1 -translate-y-1/2 text-muted-foreground"
                  onClick={() => setQuery(null)}
                >
                  <X />
                </Button>
              </div>
            )}
          </>
        )}
        {shown.length > 0 && (
          <>
            <ul className="mt-5" data-testid="mailbox-list">
              {shown.map((m) => (
                <ItemRow
                  key={m.itemId}
                  item={m}
                  now={now}
                  current={m.itemId === openId}
                  onOpen={() =>
                    listViews.setMailboxOpen({
                      nodeDid: selectedNodeDid,
                      itemId: m.itemId,
                      at: shown.indexOf(m),
                      where: MAILBOX,
                    })
                  }
                />
              ))}
            </ul>
          </>
        )}
        {items.length > 0 && searched.length > 0 && shown.length === 0 && (
          <p
            className="mt-8 text-sm text-muted-foreground"
            data-testid="mailbox-tab-empty"
          >
            Nothing here.
          </p>
        )}
        {items.length > 0 && searched.length === 0 && (
          <p
            className="mt-8 text-sm text-muted-foreground"
            data-testid="mailbox-no-match"
          >
            Nothing matches.{" "}
            <button
              type="button"
              className="underline decoration-border underline-offset-4 hover:text-foreground"
              onClick={() => setQuery(null)}
            >
              Clear the search
            </button>
          </p>
        )}
        {items.length === 0 && (
          <div className="grid min-h-[60vh] place-items-center animate-in fade-in-0 slide-in-from-bottom-2 duration-300 ease-out fill-mode-both motion-reduce:animate-none">
            <div className="text-center">
              <Inbox className="mx-auto size-6 text-muted-foreground" />
              <p className="mt-3 font-heading text-lg font-medium text-heading">
                Nothing needs your attention
              </p>
              <p className="mx-auto mt-2 max-w-sm text-sm text-muted-foreground">
                When an agent has a question, needs your approval, finishes or fails
                work, or flags something for you, it shows up here. To give an agent new
                work, start a session.
              </p>
              <Button
                variant="brand"
                className="mt-5"
                nativeButton={false}
                render={<a href={href({ name: "session", sessionId: null })} />}
              >
                <Plus /> Start a session
              </Button>
            </div>
          </div>
        )}
      </div>
    </ScrollArea>
  );
}

/* a step below the kit's tabs, as navigation rather than headings */
const TAB = "text-[13px]";

function Count({ n }: { n: number }) {
  return <span className="text-xs text-muted-foreground tabular-nums">{n}</span>;
}

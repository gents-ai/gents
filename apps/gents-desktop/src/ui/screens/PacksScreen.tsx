import { useCallback, useEffect } from "react";
import { Check, MoreHorizontal, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Input } from "@gents/ui/components/input";
import { Tabs, TabsList, TabsTrigger } from "@gents/ui/components/tabs";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import type { FoundPack } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { useUxPluginRecords } from "@/contrib/plugins-store";
import { useContributions } from "@/contrib/react/use-contributions";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { isLocalNode } from "@/lib/firstRun";
import { href, navigate } from "@/lib/router";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { useHomeDid, useSelectedNode } from "@/hooks/useClient";
import { UxPluginSwitch } from "./agent/UxPluginsPanel";
import { usePackActions } from "./agent/usePackActions";
import { useRegistrySearch } from "./agent/useRegistrySearch";
import { Hint } from "./Hint";
import { PackPage } from "./packs/PackPage";
import {
  MoreRow,
  PackCard,
  PackList,
  PackMeta,
  PackRowsLoading,
  Section,
} from "./packs/PackCard";
import {
  byAttention,
  elsewhere,
  localPlugins,
  matches,
  partLabel,
  partsOf,
  publisherOf,
  readInstalledPacks,
  readPackInfo,
  readRegistryAccount,
  rememberPacksAuthor,
  rememberPacksQuery,
  rememberPacksView,
  statusOf,
  text,
  useInstalledPacks,
  usePacksListView,
  useRegistryAccount,
} from "./packs/model";

/** The active node's packs: what it has installed and what each adds, the
    plugins loaded from this device's folder, and the registry to find more.
    Packs install on the desktop's own node; another node's are installed on
    that machine, so there the page only browses. */
export function PacksScreen({ item }: { item?: string }) {
  const node = useSelectedNode();
  const homeDid = useHomeDid();
  const { actions } = useApp();
  const local = node ? isLocalNode(node, homeDid) : false;
  const name = node ? (node.node.displayName ?? node.label) : "this node";
  const refresh = useCallback(() => readInstalledPacks(actions), [actions]);
  useEffect(() => {
    if (local) void refresh();
  }, [local, refresh]);
  return (
    <ScrollArea className="h-full" data-testid="packs-screen">
      <div className="mx-auto grid max-w-page gap-8 px-6 py-6">
        {item ? (
          <PackPage
            pack={item}
            node={node}
            local={local}
            name={name}
            refresh={refresh}
          />
        ) : (
          <PacksHome node={node} local={local} name={name} refresh={refresh} />
        )}
      </div>
    </ScrollArea>
  );
}

function PacksHome({
  node,
  local,
  name,
  refresh,
}: {
  node: ReturnType<typeof useSelectedNode>;
  local: boolean;
  name: string;
  refresh: () => Promise<void>;
}) {
  const { actions } = useApp();
  const { packs, unread, info } = useInstalledPacks();
  const records = useUxPluginRecords();
  const pages = useContributions(PLUGIN_PAGE_AREA);
  const { busy, install, update, remove } = usePackActions(refresh);
  const remembered = usePacksListView();
  const author = remembered.author;
  /* a publisher's packs are found by searching for its name, then kept to
     its namespace; typed words search instead and the namespace still
     narrows them */
  const registry = useRegistrySearch(remembered.query || author || "");
  const chosen = remembered.view;
  const setView = rememberPacksView;
  /* another node's packs aren't listed here, so it only has the registry */
  const view = local ? chosen : "registry";
  const query = remembered.query;
  const { setQuery: searchRegistry } = registry;
  useEffect(() => {
    searchRegistry(query || author || "");
  }, [author, query, searchRegistry]);
  const mine = local ? (packs ?? []) : [];
  const shown = mine.filter((p) => matches(p.pack, query)).sort(byAttention);
  const outdated = mine.filter((p) => p.outdated && p.latest);
  const plugins = localPlugins(records);
  const blocked = elsewhere(local, name);

  useEffect(() => {
    if (local) for (const p of packs ?? []) void readPackInfo(actions, p.pack);
  }, [actions, local, packs]);
  useEffect(() => {
    void readRegistryAccount(actions);
  }, [actions]);
  const me = useRegistryAccount();

  async function updateAll() {
    for (const p of outdated) await update(p.pack);
  }

  const { found, searched, page, hasMore, search } = registry;
  const isMine = (pack: string) => mine.some((m) => m.pack === pack);
  const elsewhereFound = found.filter((p) => !isMine(nameOf(p)));
  const fromAuthor = author ? found.filter((p) => p.namespace === author) : found;
  const listed = fromAuthor;

  return (
    <>
      <header className="grid gap-4">
        <div className="flex flex-wrap items-start gap-x-4 gap-y-2">
          <div className="grid min-w-0 flex-1 gap-1">
            <h1 className="font-heading text-lg font-medium text-heading">Packs</h1>
            <p className="text-sm text-muted-foreground">
              {local
                ? `Tools, agents, skills and pages that add to what ${name} can do, installed from the registry.`
                : `${blocked}. You can browse the registry here.`}
            </p>
          </div>
          {outdated.length > 1 && (
            <Button
              size="sm"
              variant="secondary"
              disabled={busy !== null}
              onClick={() => void updateAll()}
            >
              Update all
            </Button>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
          {local && (
            <Tabs
              value={view}
              onValueChange={(v) => setView(v as "installed" | "registry")}
              className="w-max"
            >
              <TabsList variant="line" aria-label="Show" className="min-h-9">
                <TabsTrigger value="installed" className="text-[13px]">
                  Installed
                  <span className="ml-1 text-muted-foreground tabular-nums">
                    {mine.length}
                  </span>
                </TabsTrigger>
                <TabsTrigger value="registry" className="text-[13px]">
                  Registry
                </TabsTrigger>
              </TabsList>
            </Tabs>
          )}
          <Input
            value={query}
            onChange={(e) => rememberPacksQuery(e.target.value)}
            {...SEARCH_FIELD}
            placeholder={
              view === "installed" ? "Search your packs" : "Search the registry"
            }
            aria-label="Search packs"
            className="min-w-48 flex-1"
          />
          {view === "registry" && author && (
            <button
              type="button"
              onClick={() => rememberPacksAuthor(null)}
              aria-label={`Show packs from everyone, not only ${author}`}
              className="inline-flex shrink-0 cursor-pointer items-center gap-1 rounded-full bg-raised px-2.5 py-1 text-xs hover:bg-accent"
            >
              By {author} <X className="size-3 text-muted-foreground" />
            </button>
          )}
        </div>
      </header>

      {/* one list at a time: your packs, or the registry's */}
      <section aria-label="Packs" className="grid gap-2">
        {view === "installed" && query.trim() !== "" && elsewhereFound.length > 0 && (
          <MoreRow onClick={() => setView("registry")}>
            {elsewhereFound.length} more in the registry
          </MoreRow>
        )}
        {view === "installed" &&
          (packs === null ? (
            <PackRowsLoading label="Loading installed packs" />
          ) : unread !== null ? (
            <p className="text-sm text-muted-foreground">
              Couldn't read the installed packs: {unread}
            </p>
          ) : mine.length === 0 ? (
            <p className="text-sm text-muted-foreground">No packs installed yet.</p>
          ) : shown.length === 0 ? (
            <p className="text-sm text-muted-foreground">No installed pack matches.</p>
          ) : (
            <PackList label="Installed packs">
              {shown.map((p) => {
                const read = info[p.pack];
                const status = statusOf(p);
                return (
                  <PackCard
                    key={p.pack}
                    pack={p.pack}
                    to={href({ name: "packs", item: p.pack })}
                    description={
                      read && read !== "unread" ? text(read.description) : null
                    }
                    adds={
                      node ? partsOf(node, p.pack, records, pages).map(partLabel) : []
                    }
                    status={status.text}
                    warn={status.warn}
                    badge={publisherOf(read, me)?.yours ? "Yours" : null}
                    menu={
                      <PackMenu
                        pack={p.pack}
                        busy={busy !== null}
                        items={[
                          ...(p.outdated && p.latest
                            ? [{ label: "Update", run: () => void update(p.pack) }]
                            : []),
                          {
                            label: "Remove",
                            run: () => void remove(p.pack),
                            destructive: true,
                          },
                        ]}
                      />
                    }
                    action={
                      p.outdated &&
                      p.latest && (
                        <Button
                          size="sm"
                          variant="outline"
                          disabled={busy !== null}
                          onClick={() => void update(p.pack)}
                        >
                          Update
                        </Button>
                      )
                    }
                  />
                );
              })}
            </PackList>
          ))}
        {view === "registry" && (
          <>
            {searched === null ? (
              <PackRowsLoading label="Loading the registry" />
            ) : listed.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {query.trim()
                  ? "Nothing in the registry matches."
                  : "The registry has no packs yet."}
              </p>
            ) : (
              <PackList label="Registry packs">
                {listed.map((pack) => {
                  const packName = nameOf(pack);
                  const have = isMine(packName);
                  return (
                    <PackCard
                      key={packName}
                      pack={packName}
                      to={href({ name: "packs", item: packName })}
                      description={text(pack.description)}
                      adds={[]}
                      status={text(pack.latest) ?? ""}
                      meta={
                        <PackMeta
                          keywords={keywordsOf(pack).slice(0, 2)}
                          downloads={
                            typeof pack.downloads === "number" ? pack.downloads : null
                          }
                        />
                      }
                      menu={
                        <PackMenu
                          pack={packName}
                          busy={busy !== null}
                          items={
                            blocked || have
                              ? []
                              : [
                                  {
                                    label: "Install",
                                    run: () => void install(packName),
                                  },
                                ]
                          }
                        />
                      }
                      action={
                        have ? (
                          <span className="inline-flex items-center gap-1 text-xs text-muted-foreground">
                            <Check aria-hidden className="size-3.5" />
                            Installed
                          </span>
                        ) : (
                          <Hint label={blocked ?? `Install ${packName} on this node`}>
                            <span>
                              <Button
                                size="sm"
                                variant="outline"
                                disabled={blocked !== null || busy !== null}
                                onClick={() => void install(packName)}
                              >
                                Install
                              </Button>
                            </span>
                          </Hint>
                        )
                      }
                    />
                  );
                })}
              </PackList>
            )}
            {(page > 1 || hasMore) && (
              <div className="flex items-center gap-2">
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={page === 1 || searched === true}
                  onClick={() => void search(page - 1)}
                >
                  Previous
                </Button>
                <span className="text-xs text-muted-foreground tabular-nums">
                  Page {page}
                </span>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={!hasMore || searched === true}
                  onClick={() => void search(page + 1)}
                >
                  Next
                </Button>
              </div>
            )}
          </>
        )}
      </section>

      {plugins.length > 0 && view === "installed" && query.trim() === "" && (
        <Section title="Local plugins">
          <ul className="divide-y divide-border/60">
            {plugins.map((plugin) => (
              <li key={plugin.id} className="flex items-center gap-3 py-2.5">
                <span className="grid min-w-0 flex-1">
                  <span className="truncate text-sm">{plugin.name}</span>
                  {plugin.status === "error" && plugin.error && (
                    <span className="truncate text-xs text-warning">
                      {plugin.error}
                    </span>
                  )}
                </span>
                <span className="text-xs text-muted-foreground">On this device</span>
                <UxPluginSwitch record={plugin} />
              </li>
            ))}
          </ul>
        </Section>
      )}
    </>
  );
}

const nameOf = (pack: FoundPack) => `${pack.namespace}/${pack.name}`;

const keywordsOf = (pack: FoundPack): string[] =>
  Array.isArray(pack.keywords)
    ? pack.keywords.filter((k): k is string => typeof k === "string")
    : [];

/* a pack row's ⋯ menu: open it, then what can be done to it from here */
function PackMenu({
  pack,
  busy,
  items,
}: {
  pack: string;
  busy: boolean;
  items: { label: string; run: () => void; destructive?: boolean }[];
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={`More for ${pack}`}
            className="text-muted-foreground"
          />
        }
      >
        <MoreHorizontal />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-44">
        <DropdownMenuItem onClick={() => navigate({ name: "packs", item: pack })}>
          Open
        </DropdownMenuItem>
        {items.map((item) => (
          <DropdownMenuItem
            key={item.label}
            disabled={busy}
            variant={item.destructive ? "destructive" : undefined}
            onClick={item.run}
          >
            {item.label}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

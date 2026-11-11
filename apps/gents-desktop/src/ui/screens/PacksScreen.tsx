import {
  type RefObject,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { toast } from "sonner";
import { Check, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { Tabs, TabsList, TabsTrigger } from "@gents/ui/components/tabs";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import type { FoundPack } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { useUxPluginRecords } from "@/contrib/plugins-store";
import { useContributions } from "@/contrib/react/use-contributions";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { isLocalNode } from "@/lib/firstRun";
import { href, navigate, pathFor, type Route } from "@/lib/router";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { useHomeDid, useSelectedNode } from "@/hooks/useClient";
import { inNativeShell, listenFileDrop, pickPackFile } from "../../lib/nativeShell";
import { errorText } from "../../lib/errorText";
import { UxPluginSwitch } from "./agent/UxPluginsPanel";
import { usePackActions } from "./agent/usePackActions";
import { PackInstallReview } from "./packs/PackInstallReview";
import { type PackReviewRequest, usePackPreviewRequest } from "@/lib/packReview";
import { PackItemView } from "./packs/PackItemView";
import {
  asideFor,
  keepPackPreview,
  previewPage,
  setReviewAside,
  useSetAsideReview,
} from "./packs/preview";
import { isPackFile } from "./packs/review";
import { useRegistrySearch } from "./agent/useRegistrySearch";
import { Hint } from "./Hint";
import { PackPage } from "./packs/PackPage";
import {
  MoreRow,
  PackCard,
  PackList,
  PackMenu,
  PackMeta,
  PackRowsLoading,
  Section,
} from "./packs/PackCard";
import {
  byAttention,
  elsewhere,
  localPlugins,
  keepRegistryPage,
  matches,
  packsListView,
  partLabel,
  partsOf,
  publisherOf,
  readInstalledPacks,
  readPackInfo,
  readRegistryAccount,
  rememberPacksAuthor,
  rememberPacksQuery,
  rememberPacksScroll,
  rememberPacksView,
  statusOf,
  text,
  useInstalledPacks,
  usePacksListView,
  useRegistryAccount,
} from "./packs/model";

/** The active node's packs: what it has installed and what each adds, the
    plugins loaded from this device's folder, and the registry to find more;
    one pack's page (`item`, or `review` for one from a file or a link), and
    one document it would add (`doc`). Packs install on the desktop's own
    node; another node's are installed on that machine, so there the page
    only browses. A review set aside opens again on arriving back at the
    page it was open on. */
export function PacksScreen({
  item,
  review,
  doc,
}: {
  item?: string;
  review?: string;
  doc?: string;
}) {
  const node = useSelectedNode();
  const homeDid = useHomeDid();
  const { actions } = useApp();
  const local = node ? isLocalNode(node, homeDid) : false;
  const name = node ? (node.node.displayName ?? node.label) : "this node";
  const refresh = useCallback(() => readInstalledPacks(actions), [actions]);
  useEffect(() => {
    if (local) void refresh();
  }, [local, refresh]);
  const packs = usePackActions(refresh);
  const { packs: installed } = useInstalledPacks();
  const reviewed = usePackPreviewRequest(review);
  const aside = useSetAsideReview();
  /* the review a document's page comes from */
  const docOf: PackReviewRequest | null = useMemo(
    () => (review ? reviewed : item ? { package: item, source: "registry" } : null),
    [review, reviewed, item],
  );
  const taken = asideFor(aside, docOf);
  const here: Route = {
    name: "packs",
    ...(item ? { item } : {}),
    ...(review ? { review } : {}),
    ...(doc ? { doc } : {}),
  };
  const path = pathFor(here);
  /* on arriving, not while the review is stepped out of here */
  const arrived = useRef<string | null>(null);
  const { review: open, reviewInstall } = packs;
  useEffect(() => {
    const arriving = arrived.current !== path;
    arrived.current = path;
    if (arriving && aside && !open && pathFor(aside.from) === path)
      reviewInstall(aside.request);
  }, [path, aside, open, reviewInstall]);
  const root = useRef<HTMLDivElement>(null);
  const [scroller, setScroller] = useState<HTMLElement | null>(null);
  const scrollRoot = useCallback((area: HTMLDivElement | null) => {
    setScroller(
      area?.querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]') ?? null,
    );
  }, []);
  /* an item's page opens at its top, not where its pack's page was */
  useLayoutEffect(() => {
    if (doc && scroller) scroller.scrollTop = 0;
  }, [doc, scroller]);
  const over = usePackFileDrop(root, local, (path) =>
    navigate(previewPage({ package: path, source: "file" })),
  );
  return (
    <div ref={root} className="relative h-full">
      <ScrollArea ref={scrollRoot} className="h-full" data-testid="packs-screen">
        <div className="mx-auto grid max-w-page gap-8 px-6 py-6">
          {doc && docOf ? (
            <PackItemView
              request={docOf}
              doc={doc}
              node={node}
              resume={taken ? () => packs.reviewInstall(taken.request) : null}
            />
          ) : review ? (
            reviewed ? (
              <PackPage
                pack={null}
                request={reviewed}
                node={node}
                local={local}
                name={name}
                packActions={packs}
                scroller={scroller}
              />
            ) : (
              <p className="text-sm text-muted-foreground">
                This pack isn't open any more.{" "}
                <a
                  href={href({ name: "packs" })}
                  className="underline-offset-2 hover:underline"
                >
                  Back to Packs
                </a>
              </p>
            )
          ) : item ? (
            <PackPage
              pack={item}
              node={node}
              local={local}
              name={name}
              packActions={packs}
              scroller={scroller}
            />
          ) : (
            <PacksHome
              node={node}
              local={local}
              name={name}
              packActions={packs}
              scroller={scroller}
            />
          )}
        </div>
      </ScrollArea>
      {over && (
        <div
          aria-hidden
          className="pointer-events-none absolute inset-3 grid place-items-center rounded-xl border-2 border-dashed border-ring/60 bg-background/80 text-sm"
        >
          Drop a .pack file to review and install it
        </div>
      )}
      <PackInstallReview
        key={packs.opened}
        request={packs.review}
        node={local ? node : null}
        installed={installed}
        resume={asideFor(aside, packs.review)}
        onCancel={packs.closeReview}
        onInspect={(ref, set) => {
          setReviewAside({ ...set, from: here });
          keepPackPreview(set.request, set.report);
          packs.setReviewAside();
          navigate(previewPage(set.request, ref));
        }}
        onInstall={async (request, choice) => {
          await packs.installReviewed(request, choice);
          /* what it would add, from a file or a link, is now what it added */
          if (review || doc) navigate({ name: "packs", item: choice.pack });
        }}
      />
    </div>
  );
}

/** The packs actions a Packs page shares, with the review every install
    passes through. */
export type PackActions = ReturnType<typeof usePackActions>;

/* A `.pack` file dropped on the page opens its review; other files are
   refused by name. Only the desktop's own node installs packs. True while a
   file is held over the page. */
function usePackFileDrop(
  root: RefObject<HTMLDivElement | null>,
  local: boolean,
  review: (path: string) => void,
) {
  const [over, setOver] = useState(false);
  const latest = useRef(review);
  latest.current = review;
  useEffect(() => {
    if (!local) return;
    let disposed = false;
    let stop: (() => void) | undefined;
    void listenFileDrop((event) => {
      if (disposed) return;
      const rect = root.current?.getBoundingClientRect();
      const inside =
        !!rect &&
        event.x >= rect.left &&
        event.x <= rect.right &&
        event.y >= rect.top &&
        event.y <= rect.bottom;
      setOver(inside && event.type !== "leave" && event.type !== "drop");
      if (!inside || event.type !== "drop") return;
      const paths = event.paths ?? [];
      const pack = paths.find(isPackFile);
      if (pack) latest.current(pack);
      else if (paths.length) toast.error("Only a .pack file installs here");
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else stop = unlisten;
      })
      .catch((error) => console.warn("could not listen for dropped packs", error));
    return () => {
      disposed = true;
      stop?.();
      setOver(false);
    };
  }, [root, local]);
  return over;
}

/* the person picks a `.pack` file and reviews installing it */
async function installFromFile(review: (path: string) => void) {
  try {
    const path = await pickPackFile();
    if (path) review(path);
  } catch (error) {
    toast.error(`Couldn't open the file: ${errorText(error)}`);
  }
}

/* Each list is put back where it was scrolled to once its rows are drawn,
   and every scroll after is kept. Listening in a layout effect stops before
   the list's rows are taken away, so the scroll that clamps doesn't count. */
function useKeptScroll(
  scroller: HTMLElement | null,
  view: "installed" | "registry",
  drawn: boolean,
) {
  const pending = useRef<number | null>(null);
  const shown = useRef<string | null>(null);
  useLayoutEffect(() => {
    if (shown.current !== view) {
      shown.current = view;
      pending.current = packsListView().scroll[view];
    }
    if (!scroller || !drawn || pending.current === null) return;
    scroller.scrollTop = pending.current;
    pending.current = null;
  }, [scroller, view, drawn]);
  useLayoutEffect(() => {
    if (!scroller) return;
    const keep = () => {
      if (pending.current === null) rememberPacksScroll(view, scroller.scrollTop);
    };
    scroller.addEventListener("scroll", keep, { passive: true });
    return () => scroller.removeEventListener("scroll", keep);
  }, [scroller, view]);
}

function PacksHome({
  node,
  local,
  name,
  packActions,
  scroller,
}: {
  node: ReturnType<typeof useSelectedNode>;
  local: boolean;
  name: string;
  packActions: PackActions;
  /** the page's scrolling area */
  scroller: HTMLElement | null;
}) {
  const { actions } = useApp();
  const { packs, unread, info } = useInstalledPacks();
  const records = useUxPluginRecords();
  const pages = useContributions(PLUGIN_PAGE_AREA);
  const { busy, install, update, remove } = packActions;
  const remembered = usePacksListView();
  const author = remembered.author;
  /* a publisher's packs are found by searching for its name, then kept to
     its namespace; typed words search instead and the namespace still
     narrows them */
  const registry = useRegistrySearch(remembered.query || author || "", {
    last: packsListView().registry,
    save: keepRegistryPage,
  });
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
  useKeptScroll(
    scroller,
    view,
    view === "installed" ? !local || packs !== null : searched !== null,
  );
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
                ? `Tools, agents, skills and pages that add to what ${name} can do, installed from the registry or a .pack file.`
                : `${blocked}. You can browse the registry here.`}
            </p>
          </div>
          {local && inNativeShell() && (
            <Button
              size="sm"
              variant="outline"
              disabled={busy !== null}
              onClick={() =>
                void installFromFile((path) =>
                  navigate(previewPage({ package: path, source: "file" })),
                )
              }
            >
              Install from file…
            </Button>
          )}
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
              <TabsList
                variant="line"
                aria-label="Show"
                className="-ms-1 min-h-9 gap-4"
              >
                <TabsTrigger value="installed" className="px-0 text-[13px]">
                  Installed
                  <span className="ml-1 text-muted-foreground tabular-nums">
                    {mine.length}
                  </span>
                </TabsTrigger>
                <TabsTrigger value="registry" className="px-0 text-[13px]">
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
                        opens
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
                  const latest = text(pack.latest);
                  return (
                    <PackCard
                      key={packName}
                      pack={packName}
                      to={href({ name: "packs", item: packName })}
                      description={text(pack.description)}
                      adds={[]}
                      status={latest ?? ""}
                      statusClassName="w-14 max-w-none min-w-0"
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
                          opens
                          pack={packName}
                          busy={busy !== null}
                          items={
                            blocked || have || !latest
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
                        ) : !latest ? (
                          <span
                            className="text-xs text-muted-foreground"
                            title="Its publisher hasn't published a version yet"
                          >
                            No version
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

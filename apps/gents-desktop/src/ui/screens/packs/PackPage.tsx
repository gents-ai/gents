import { type ReactNode, useEffect, useLayoutEffect, useMemo, useRef } from "react";
import { ArrowLeft, ArrowRight, ChevronRight } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { Spinner } from "@gents/ui/components/spinner";
import { useApp } from "@/app/AppContext";
import { useUxPluginRecords } from "@/contrib/plugins-store";
import { useContributions } from "@/contrib/react/use-contributions";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { href, navigate, type Route } from "@/lib/router";
import type { NodeView } from "../../../hooks/fleetStore";
import { UxPluginSwitch } from "../agent/UxPluginsPanel";
import type { PackActions } from "../PacksScreen";
import { Hint } from "../Hint";
import { AgentInitials } from "../parts";
import { isListed, SECTIONS } from "../agent/sections";
import { canOpenPageWindows, openPageWindow } from "../PluginPageScreen";
import { IS_MAC } from "../shortcutKeys";
import { CARD, PackMenu, PackMeta, Section } from "./PackCard";
import { isExternalUrl } from "../../../lib/externalLinks";
import { copyText } from "../../lib/clipboard";
import { isPackName, packLink, type PackReviewRequest } from "@/lib/packReview";
import { packSettings } from "../agent/packContents";
import {
  type Resource,
  type ResourceTree,
  bytes,
  elsewhere,
  namespaceOf,
  nodeLinks,
  otherLabel,
  pageOf,
  partLabel,
  partsOf,
  pluginsOf,
  publisherOf,
  readPackInfo,
  readRegistryAccount,
  resourceTree,
  shortName,
  showPublisher,
  sizeOf,
  statusOf,
  summaryOf,
  text,
  keepPackPlace,
  packPlaceTop,
  useInstalledPacks,
  usePackPlace,
  useRegistryAccount,
  versionsOf,
} from "./model";
import {
  asideFor,
  docRef,
  previewCounts,
  previewPage,
  previewTree,
  readPackPreview,
  requestedName,
  requestedPack,
  usePackPreview,
  useSetAsideReview,
} from "./preview";

const day = (at: string) =>
  new Date(at).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
  });

/* an agent's initials, as the composer's picker shows it; anything else,
   its configuration section's icon */
export function ItemIcon({ section, name }: { section: string; name: string }) {
  if (section === "agents")
    return <AgentInitials name={name} className="size-6 text-[10px]" />;
  const found = SECTIONS.find((s) => s.id === section);
  const Icon = found && isListed(found) ? found.icon : null;
  /* a kind with no section keeps the column, so names line up */
  return (
    <span className="grid size-6 shrink-0 place-items-center">
      {Icon && <Icon className="size-4 text-muted-foreground" />}
    </span>
  );
}

const ROW_BOX =
  "flex items-center rounded-lg transition-colors has-[a:hover]:bg-accent has-[a:focus-visible]:ring-2 has-[a:focus-visible]:ring-ring/50";

/* one thing the pack adds: the whole row opens its page, or, before it's
   installed, its own read-only view; a plugin is named only. `caret`, at
   the top of a tree with rows that open, takes the row's first column. */
function ResourceRow({
  resource: r,
  to,
  caret,
  label,
}: {
  resource: Resource;
  to: string | null;
  caret?: ReactNode;
  /** what the row says on its right, in place of its kind */
  label?: string;
}) {
  const inner = (
    <>
      <ItemIcon section={r.section} name={r.name} />
      <span className="min-w-0 flex-1 truncate">{r.name}</span>
      <span className="shrink-0 text-xs text-muted-foreground">
        {label ?? (r.relation ? `${r.kind} · ${r.relation}` : r.kind)}
      </span>
    </>
  );
  const row = `flex min-w-0 flex-1 items-center gap-3 py-2 text-sm ${caret === undefined ? "px-3" : "pr-3 pl-1"}`;
  return (
    <div className={ROW_BOX}>
      {caret}
      {to ? (
        <a href={to} title={r.id} className={`${row} outline-none`}>
          {inner}
        </a>
      ) : (
        <div className={row}>{inner}</div>
      )}
    </div>
  );
}

type Opens = (r: Resource, parent: Resource | null) => string | null;

const keyOf = (r: Resource) => `${r.collection ?? r.section}:${r.id}`;
const rowsIn = (tree: readonly Resource[]): number =>
  tree.reduce((n, r) => n + 1 + rowsIn(r.children), 0);

/* past this many rows, fully open, a tree starts with its top rows closed */
const OPEN_ROWS = 12;

/* what a pack adds, each agent with what's tied to it nested under it; a
   top row with rows under it opens and closes, and says what's under it
   while closed */
function ResourceList({
  tree,
  to,
  place,
  parent = null,
}: {
  tree: readonly Resource[];
  to: Opens;
  /** the pack's page these rows belong to */
  place: string;
  parent?: Resource | null;
}) {
  const { shut } = usePackPlace(place);
  if (parent)
    return (
      <ul className="mb-2 ml-3 border-l border-border/60 pl-3">
        {tree.map((r) => (
          <li key={keyOf(r)}>
            <ResourceRow resource={r} to={to(r, parent)} />
            {r.children.length > 0 && (
              <ResourceList tree={r.children} to={to} place={place} parent={r} />
            )}
          </li>
        ))}
      </ul>
    );
  const opens = tree.some((r) => r.children.length > 0);
  const closed = rowsIn(tree) > OPEN_ROWS;
  return (
    <ul className="divide-y divide-border/60">
      {tree.map((r) => {
        const key = keyOf(r);
        const open = r.children.length > 0 && !(shut[key] ?? closed);
        const caret = !opens ? undefined : r.children.length ? (
          <button
            type="button"
            aria-expanded={open}
            aria-label={`What's under ${r.name}`}
            onClick={() => keepPackPlace(place, { shut: { ...shut, [key]: open } })}
            className="ml-[7px] grid size-6 shrink-0 cursor-pointer place-items-center rounded-md text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <ChevronRight
              className={`size-3.5 transition-transform duration-150 motion-reduce:transition-none ${open ? "rotate-90" : ""}`}
            />
          </button>
        ) : (
          <span aria-hidden className="ml-[7px] size-6 shrink-0" />
        );
        return (
          <li key={key}>
            <ResourceRow
              resource={r}
              to={to(r, null)}
              caret={caret}
              label={
                r.children.length && !open
                  ? [r.kind, ...summaryOf(r.children)].join(" · ")
                  : undefined
              }
            />
            {open && (
              <ResourceList tree={r.children} to={to} place={place} parent={r} />
            )}
          </li>
        );
      })}
    </ul>
  );
}

/* The page is put back where it was scrolled to once its rows are drawn, and
   every scroll after is kept. Listening in a layout effect stops before the
   rows are taken away, so the scroll that clamps doesn't count. */
function useKeptPackTop(scroller: HTMLElement | null, place: string, drawn: boolean) {
  const restored = useRef<string | null>(null);
  useLayoutEffect(() => {
    if (!scroller || !drawn || restored.current === place) return;
    restored.current = place;
    scroller.scrollTop = packPlaceTop(place);
  }, [scroller, place, drawn]);
  useLayoutEffect(() => {
    if (!scroller) return;
    const keep = () => {
      if (restored.current === place) keepPackPlace(place, { top: scroller.scrollTop });
    };
    scroller.addEventListener("scroll", keep, { passive: true });
    return () => scroller.removeEventListener("scroll", keep);
  }, [scroller, place]);
}

/* what a pack adds, then, collapsed, the settings that belong to nothing
   else it adds */
function Resources({
  tree,
  other,
  to,
  place,
}: ResourceTree & { to: Opens; place: string }) {
  const { other: otherOpen } = usePackPlace(place);
  return (
    <div className="grid">
      <ResourceList tree={tree} to={to} place={place} />
      {other.length > 0 && (
        <Collapsible
          open={otherOpen}
          onOpenChange={(open) => keepPackPlace(place, { other: open })}
          className={`group/other ${tree.length ? "border-t border-border/60" : ""}`}
        >
          <CollapsibleTrigger className="flex w-full cursor-pointer items-center gap-2 rounded-lg px-3 py-2 text-left text-sm hover:bg-accent">
            <ChevronRight className="size-3.5 shrink-0 text-muted-foreground transition-transform duration-150 group-data-open/other:rotate-90 motion-reduce:transition-none" />
            <span className="flex-1">Other settings</span>
            <span className="text-xs text-muted-foreground tabular-nums">
              {other.length}
            </span>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <ResourceList tree={other} to={to} place={place} />
          </CollapsibleContent>
        </Collapsible>
      )}
    </div>
  );
}

/* where the node's configuration shows something a pack added: a
   compaction in the contexts that use it, a target in the tool set that
   calls it */
function configRoute(nodeDid: string, r: Resource, parent: Resource | null): Route {
  const at = { name: "agent" as const, nodeDid };
  if (r.section === "compactions") return { ...at, section: "contexts" };
  if (r.section === "agent-targets")
    return parent?.section === "tools"
      ? { ...at, section: "tools", item: parent.id }
      : { ...at, section: "tools" };
  return { ...at, section: r.section, item: r.id };
}

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];

const ROW =
  "flex items-center gap-3 border-b border-border/40 px-3 py-2 text-sm last:border-0";

/** One pack's own page: what it is and how it stands on the active node,
    what it added (each kind a link to where the node's configuration lists
    it), the pages its plugins draw, its plugins' switches, and its
    versions. A pack the node hasn't installed, and one from a file or a
    link (`request`), shows what it would add as its install's preview
    reports it, each document a link to its own read-only page, with
    Install; a review stepped out of is taken back to from here. */
export function PackPage({
  pack: named,
  request,
  node,
  local,
  name,
  packActions,
  scroller,
}: {
  /** the pack, by its name; a file's isn't known until it's read */
  pack: string | null;
  /** the file or link it's previewed from */
  request?: PackReviewRequest | null;
  node: NodeView | null;
  local: boolean;
  name: string;
  packActions: PackActions;
  /** what scrolls the page */
  scroller: HTMLElement | null;
}) {
  const { actions } = useApp();
  const { packs, info } = useInstalledPacks();
  const records = useUxPluginRecords();
  const pages = useContributions(PLUGIN_PAGE_AREA);
  const { busy, update, remove, reviewInstall } = packActions;
  const fromFile = request?.source === "file";
  const listedPack = named ?? (request ? requestedPack(request) : null);
  const installedListed = local ? packs?.find((p) => p.pack === listedPack) : undefined;
  /* a registry pack, named as the registry names packs, is previewed once
     the node's own packs are known not to hold it */
  const previewing: PackReviewRequest | null = useMemo(
    () =>
      request ??
      (listedPack &&
      listedPack.split("/").length === 2 &&
      listedPack.split("/").every(isPackName) &&
      (!local || packs !== null) &&
      !installedListed
        ? { package: listedPack, source: "registry" }
        : null),
    [request, listedPack, local, packs, installedListed],
  );
  const preview = usePackPreview(previewing);
  useEffect(() => {
    if (previewing) void readPackPreview(actions, previewing);
  }, [actions, previewing]);
  const report = preview?.state === "read" ? preview.report : null;
  const pack = report?.coordinate ?? listedPack ?? "";
  const place = request?.package ?? named ?? pack;
  useKeptPackTop(scroller, place, preview?.state !== "reading");
  useEffect(() => {
    if (!pack || fromFile) return;
    void readPackInfo(actions, pack);
  }, [actions, pack, fromFile]);
  useEffect(() => {
    void readRegistryAccount(actions);
  }, [actions]);
  const me = useRegistryAccount();
  const aside = asideFor(useSetAsideReview(), previewing);
  const read = fromFile ? undefined : info[pack];
  const registry = read && read !== "unread" ? read : undefined;
  const installed = local ? packs?.find((p) => p.pack === pack) : undefined;
  const status = installed ? statusOf(installed) : null;
  const shown = installed && !previewing;
  const parts = shown && node ? partsOf(node, pack, records, pages) : [];
  const plugins = shown ? pluginsOf(records, pack) : [];
  const withPages = plugins.filter((p) => pageOf(pages, p.id));
  const versions = versionsOf(registry);
  const latest = text(registry?.latest) ?? versions[0]?.version ?? null;
  const version = report?.version ?? latest;
  const blocked = elsewhere(local, name);
  const description = text(registry?.description) ?? text(report?.description);
  const namespace = namespaceOf(pack);
  /* what it added that the configuration lists; its pages have a section
     of their own */
  const listed = node ? parts.filter((part) => part.section !== null) : [];
  const added: ResourceTree =
    shown && node
      ? resourceTree(parts, packSettings(node, pack), nodeLinks(node))
      : { tree: [], other: [] };
  const would = report ? previewTree(report) : null;
  const publisher = publisherOf(read, me);
  const page = text(registry?.homepage);
  const homepage = page && isExternalUrl(page) ? page : null;
  const keywords = strings(registry?.keywords);
  const downloads = typeof registry?.downloads === "number" ? registry.downloads : null;
  const newest = versions.find((v) => v.version === latest) ?? versions[0];
  const published = newest?.publishedAt ?? null;
  const size = sizeOf(registry as { versions?: unknown } | undefined, latest);
  const title = pack ? shortName(pack) : request ? requestedName(request) : "";

  return (
    <article className="grid gap-8" data-testid="pack-page">
      <a
        href={href({ name: "packs" })}
        className="inline-flex w-fit items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="size-3" /> Packs
      </a>
      <header className="flex items-start gap-4">
        <div className="grid min-w-0 flex-1 gap-1">
          <h1 className="font-heading text-lg font-medium text-heading">{title}</h1>
          <p className="flex flex-wrap items-center gap-x-1 text-xs text-muted-foreground tabular-nums">
            {namespace && (
              <>
                <button
                  type="button"
                  onClick={() => {
                    showPublisher(namespace);
                    navigate({ name: "packs" });
                  }}
                  title={`Packs by ${namespace}`}
                  className="cursor-pointer underline-offset-2 hover:text-foreground hover:underline"
                >
                  {namespace}
                </button>
                <span aria-hidden>·</span>
              </>
            )}
            <span>
              {[
                status && !previewing
                  ? status.text
                  : installed
                    ? [version, `${installed.installed} installed`]
                        .filter(Boolean)
                        .join(" · ")
                    : version
                      ? `Not installed · ${version}`
                      : "Not installed",
                fromFile && `From ${requestedName(request!)}`,
                request?.source === "link" && "From a link",
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
          </p>
          {publisher ? (
            <p className="text-xs text-muted-foreground">
              Published by {publisher.yours ? "you" : publisher.name}
            </p>
          ) : (
            read === "unread" &&
            !request && (
              <p className="text-xs text-muted-foreground">
                The registry has no record of it, so it may have come from a file or a
                link
              </p>
            )
          )}
          {description && <p className="mt-1 max-w-prose text-sm">{description}</p>}
          <PackMeta keywords={keywords} downloads={downloads} className="mt-1.5" />
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {previewing ? (
            aside ? (
              <Button
                size="sm"
                variant="brand"
                onClick={() => reviewInstall(aside.request)}
              >
                Back to install
              </Button>
            ) : (
              <Hint label={blocked ?? `Install ${pack || title} on this node`}>
                <span>
                  <Button
                    size="sm"
                    variant="brand"
                    disabled={blocked !== null || busy !== null}
                    onClick={() => reviewInstall(previewing)}
                  >
                    Install
                  </Button>
                </span>
              </Hint>
            )
          ) : (
            installed?.outdated && (
              <Button
                size="sm"
                variant="outline"
                disabled={busy !== null}
                onClick={() => void update(pack)}
              >
                Update
              </Button>
            )
          )}
          {installed && (
            <PackMenu
              pack={pack}
              busy={busy !== null}
              items={[
                {
                  label: "Copy a link",
                  run: () =>
                    void copyText(
                      packLink(pack, installed.installed, installed.digest),
                    ),
                },
                {
                  label: "Remove",
                  destructive: true,
                  run: () =>
                    void remove(pack).then(
                      (removed) => removed && navigate({ name: "packs" }),
                    ),
                },
              ]}
            />
          )}
        </div>
      </header>

      {registry && (published || size || homepage) && (
        <Section title="About">
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-1.5 text-sm">
            {published && (
              <>
                <dt className="text-muted-foreground">Latest</dt>
                <dd className="tabular-nums">
                  {[latest, `published ${day(published)}`, size && bytes(size)]
                    .filter(Boolean)
                    .join(" · ")}
                </dd>
              </>
            )}
            {homepage && (
              <>
                <dt className="text-muted-foreground">Homepage</dt>
                <dd className="min-w-0 truncate">
                  <a
                    href={homepage}
                    target="_blank"
                    rel="noreferrer"
                    className="underline-offset-2 hover:underline"
                  >
                    {homepage.replace(/^https?:\/\//, "")}
                  </a>
                </dd>
              </>
            )}
          </dl>
        </Section>
      )}

      {added.tree.length + added.other.length > 0 && (
        <Section
          title="What it adds"
          action={
            <span className="text-xs text-muted-foreground">
              {[
                ...listed.map(partLabel),
                ...(added.other.length ? [otherLabel(added.other.length)] : []),
              ].join(" · ")}
            </span>
          }
        >
          <Resources
            {...added}
            place={place}
            to={(r, parent) => href(configRoute(node!.nodeDid, r, parent))}
          />
        </Section>
      )}

      {previewing && preview && (
        <Section
          title="What it would add"
          action={
            report && (
              <span className="text-xs text-muted-foreground">
                {previewCounts(report).join(" · ")}
              </span>
            )
          }
        >
          {preview.state === "reading" ? (
            <p className="flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Reading the pack
            </p>
          ) : preview.state === "unread" ? (
            <p role="alert" className="text-sm text-warning">
              Couldn't read what it would add.{" "}
              <span className="text-muted-foreground">{preview.error}</span>
            </p>
          ) : would && would.tree.length + would.other.length > 0 ? (
            <Resources
              {...would}
              place={place}
              to={(r) =>
                r.collection
                  ? href(previewPage(previewing, docRef(r.collection, r.id)))
                  : null
              }
            />
          ) : (
            <p className="text-sm text-muted-foreground">
              {report?.documents
                ? "No documents"
                : "Not reported for this kind of pack"}
            </p>
          )}
        </Section>
      )}

      {withPages.length > 0 && (
        <Section title="Pages">
          <ul className="grid">
            {withPages.map((plugin) => {
              const windows = canOpenPageWindows();
              const title = pageOf(pages, plugin.id)?.title ?? plugin.name;
              return (
                <li key={plugin.id} className={CARD}>
                  <a
                    href={href({ name: "plugin", pluginId: plugin.id })}
                    title={
                      windows
                        ? `${IS_MAC ? "⌘" : "Ctrl"}-click to open in a new window`
                        : undefined
                    }
                    onClick={(event) => {
                      if (!windows || !(event.metaKey || event.ctrlKey)) return;
                      event.preventDefault();
                      openPageWindow(plugin.id, plugin.name, node?.nodeDid ?? null);
                    }}
                    className="min-w-0 flex-1 truncate text-sm outline-none after:absolute after:inset-0 after:rounded-lg"
                  >
                    {title}
                  </a>
                  <ArrowRight
                    aria-hidden
                    className="size-4 shrink-0 text-muted-foreground"
                  />
                </li>
              );
            })}
          </ul>
        </Section>
      )}

      {plugins.length > 0 && (
        <Section title="Interface plugins">
          <ul className="grid">
            {plugins.map((plugin) => (
              <li key={plugin.id} className={ROW}>
                <span className="grid min-w-0 flex-1">
                  <span className="truncate">{plugin.name}</span>
                  {plugin.status === "error" && plugin.error ? (
                    <span className="truncate text-xs text-warning">
                      {plugin.error}
                    </span>
                  ) : (
                    plugin.description && (
                      <span className="line-clamp-2 max-w-prose text-xs text-muted-foreground">
                        {plugin.description}
                      </span>
                    )
                  )}
                </span>
                <span className="text-xs text-muted-foreground">On this device</span>
                <UxPluginSwitch record={plugin} />
              </li>
            ))}
          </ul>
        </Section>
      )}

      {versions.length > 0 && (
        <Section title="Versions">
          <ul className="grid">
            {versions.map((v) => (
              <li key={v.version} className={ROW}>
                <span className="w-20 shrink-0 tabular-nums">{v.version}</span>
                <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                  {[
                    v.publishedAt && day(v.publishedAt),
                    installed?.installed === v.version && "Installed",
                    v.version === latest && "Latest",
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </span>
              </li>
            ))}
          </ul>
        </Section>
      )}
    </article>
  );
}

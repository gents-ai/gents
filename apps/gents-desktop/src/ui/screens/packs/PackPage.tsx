import { useEffect } from "react";
import { ArrowLeft, SquareArrowOutUpRight } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { useApp } from "@/app/AppContext";
import { useUxPluginRecords } from "@/contrib/plugins-store";
import { useContributions } from "@/contrib/react/use-contributions";
import { PLUGIN_PAGE_AREA } from "@/contrib/types";
import { href, navigate } from "@/lib/router";
import type { NodeView } from "../../../hooks/fleetStore";
import { UxPluginSwitch } from "../agent/UxPluginsPanel";
import { usePackActions } from "../agent/usePackActions";
import { Hint } from "../Hint";
import { AgentInitials } from "../parts";
import { isListed, SECTIONS } from "../agent/sections";
import { canOpenPageWindows, openPageWindow } from "../PluginPageScreen";
import { CARD, PackMark, PackMeta, Section } from "./PackCard";
import { isExternalUrl } from "../../../lib/externalLinks";
import {
  type Resource,
  elsewhere,
  namespaceOf,
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
  statusOf,
  text,
  useInstalledPacks,
  useRegistryAccount,
  versionsOf,
} from "./model";

const day = (at: string) =>
  new Date(at).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
  });

/* an agent's initials, as the composer's picker shows it; anything else,
   its configuration section's icon */
function ItemIcon({ section, name }: { section: string; name: string }) {
  if (section === "agents")
    return <AgentInitials name={name} className="size-6 text-[10px]" />;
  const found = SECTIONS.find((s) => s.id === section);
  const Icon = found && isListed(found) ? found.icon : null;
  return Icon ? (
    <span className="grid size-6 shrink-0 place-items-center">
      <Icon className="size-4 text-muted-foreground" />
    </span>
  ) : null;
}

/* one thing the pack added: the whole row opens its settings */
function ResourceRow({
  resource: r,
  nodeDid,
}: {
  resource: Resource;
  nodeDid: string;
}) {
  return (
    <a
      href={href({ name: "agent", nodeDid, section: r.section, item: r.id })}
      title={r.id}
      className="-mx-3 flex items-center gap-3 rounded-lg px-3 py-2 text-sm transition-colors outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring/50"
    >
      <ItemIcon section={r.section} name={r.name} />
      <span className="min-w-0 flex-1 truncate">{r.name}</span>
      <span className="shrink-0 text-xs text-muted-foreground">
        {r.relation ? `${r.kind} · ${r.relation}` : r.kind}
      </span>
    </a>
  );
}

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];

/* the latest version's size, as the registry lists it */
function sizeOf(
  info: { versions?: unknown } | undefined | null,
  latest: string | null,
) {
  if (!info || !Array.isArray(info.versions)) return null;
  const v = info.versions.find(
    (x) =>
      typeof x === "object" &&
      x !== null &&
      (x as { version?: unknown }).version === latest,
  ) as { size_bytes?: unknown } | undefined;
  return typeof v?.size_bytes === "number" ? v.size_bytes : null;
}

function bytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

const ROW =
  "flex items-center gap-3 border-b border-border/40 py-2 text-sm last:border-0";

/** One pack's own page: what it is and how it stands on the active node,
    what it added (each kind a link to where the node's configuration lists
    it), the pages its plugins draw, its plugins' switches, and its
    versions. A pack the node hasn't installed shows the registry's page
    with Install. */
export function PackPage({
  pack,
  node,
  local,
  name,
  refresh,
}: {
  pack: string;
  node: NodeView | null;
  local: boolean;
  name: string;
  refresh: () => Promise<void>;
}) {
  const { actions } = useApp();
  const { packs, info } = useInstalledPacks();
  const records = useUxPluginRecords();
  const pages = useContributions(PLUGIN_PAGE_AREA);
  const { busy, install, update, remove } = usePackActions(refresh);
  useEffect(() => {
    void readPackInfo(actions, pack);
    void readRegistryAccount(actions);
  }, [actions, pack]);
  const me = useRegistryAccount();
  const read = info[pack];
  const registry = read && read !== "unread" ? read : undefined;
  const installed = local ? packs?.find((p) => p.pack === pack) : undefined;
  const status = installed ? statusOf(installed) : null;
  const parts = installed && node ? partsOf(node, pack, records, pages) : [];
  const plugins = installed ? pluginsOf(records, pack) : [];
  const withPages = plugins.filter((p) => pageOf(pages, p.id));
  const versions = versionsOf(registry);
  const latest = text(registry?.latest) ?? versions[0]?.version ?? null;
  const blocked = elsewhere(local, name);
  const description = text(registry?.description);
  const namespace = namespaceOf(pack);
  /* what it added that the configuration lists; its pages have a section
     of their own */
  const listed = node ? parts.filter((part) => part.section !== null) : [];
  const tree = node ? resourceTree(node, parts) : [];
  const publisher = publisherOf(read, me);
  const page = text(registry?.homepage);
  const homepage = page && isExternalUrl(page) ? page : null;
  const keywords = strings(registry?.keywords);
  const downloads = typeof registry?.downloads === "number" ? registry.downloads : null;
  const newest = versions.find((v) => v.version === latest) ?? versions[0];
  const published = newest?.publishedAt ?? null;
  const size = sizeOf(registry as { versions?: unknown } | undefined, latest);

  return (
    <article className="grid gap-8" data-testid="pack-page">
      <a
        href={href({ name: "packs" })}
        className="inline-flex w-fit items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="size-3" /> Packs
      </a>
      <header className="flex items-start gap-4">
        <PackMark pack={pack} large />
        <div className="grid min-w-0 flex-1 gap-1">
          <h1 className="font-heading text-lg font-medium text-heading">
            {shortName(pack)}
          </h1>
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
              {status
                ? status.text
                : latest
                  ? `Not installed · ${latest}`
                  : "Not installed"}
            </span>
          </p>
          {publisher ? (
            <p className="text-xs text-muted-foreground">
              Published by {publisher.yours ? "you" : publisher.name}
            </p>
          ) : (
            read === "unread" && (
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
          {installed ? (
            <>
              {installed.outdated && (
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy !== null}
                  onClick={() => void update(pack)}
                >
                  Update
                </Button>
              )}
              <Button
                size="sm"
                variant="ghost"
                disabled={busy !== null}
                onClick={() =>
                  void remove(pack).then(
                    (removed) => removed && navigate({ name: "packs" }),
                  )
                }
              >
                Remove
              </Button>
            </>
          ) : (
            <Hint label={blocked ?? `Install ${pack} on this node`}>
              <span>
                <Button
                  size="sm"
                  variant="brand"
                  disabled={blocked !== null || busy !== null}
                  onClick={() => void install(pack)}
                >
                  Install
                </Button>
              </span>
            </Hint>
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

      {tree.length > 0 && (
        <Section
          title="What it adds"
          action={
            <span className="text-xs text-muted-foreground">
              {listed.map(partLabel).join(" · ")}
            </span>
          }
        >
          <ul className="divide-y divide-border/60">
            {tree.map((r) => (
              <li key={`${r.section}:${r.id}`}>
                <ResourceRow resource={r} nodeDid={node!.nodeDid} />
                {r.children.length > 0 && (
                  <ul className="mb-2 ml-3 border-l border-border/60 pl-3">
                    {r.children.map((c) => (
                      <li key={`${c.section}:${c.id}`}>
                        <ResourceRow resource={c} nodeDid={node!.nodeDid} />
                      </li>
                    ))}
                  </ul>
                )}
              </li>
            ))}
          </ul>
        </Section>
      )}

      {withPages.length > 0 && (
        <Section title="Pages">
          <ul className="grid">
            {withPages.map((plugin) => {
              const title = pageOf(pages, plugin.id)?.title ?? plugin.name;
              return (
                <li key={plugin.id} className={CARD}>
                  <a
                    href={href({ name: "plugin", pluginId: plugin.id })}
                    className="min-w-0 flex-1 truncate text-sm outline-none after:absolute after:inset-0 after:rounded-lg"
                  >
                    {title}
                  </a>
                  {canOpenPageWindows() && (
                    <span className="relative z-10">
                      <Hint label="Open in new window">
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          aria-label={`Open ${title} in new window`}
                          onClick={() =>
                            openPageWindow(
                              plugin.id,
                              plugin.name,
                              node?.nodeDid ?? null,
                            )
                          }
                        >
                          <SquareArrowOutUpRight />
                        </Button>
                      </Hint>
                    </span>
                  )}
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

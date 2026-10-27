/* The runtime doors from the webview: ask the bridge what is on disk
   (`<home>/ux-plugins/<id>/` and every installed pack's ux[]), fetch each
   module's source, and push it through the loader. A manual Reload re-reads
   every entry (an installer that replaces a folder atomically keeps the
   same path). Rust owns the scan; this owns the lifecycle. */
import type { UxPluginListing, UxPluginSource } from "@source-inc/gents-desktop-client";
import { loadRuntimeUxPlugin, retireRuntimeUxPlugin } from "./loader";
import { publishUxPlugin, uxPluginActive, uxPluginRecords } from "./plugins-store";

/* the listing and the source as the client's adapter types them; the door
   narrows nothing further */
export type { UxPluginListing, UxPluginSource };

/* live runtime plugins by listing id -> loaded plugin id (null while broken
   or shadowed) */
const known = new Map<string, string | null>();
/* plugin id -> the listing that owns it. Two packs may ship a plugin of one
   name; the first listing (scan order) owns the id whatever its state, and
   the other is shown as shadowed until the owner leaves. */
const owners = new Map<string, string>();
/* the scan in flight; a scan asked for meanwhile runs after it */
let inFlight: Promise<void> | null = null;

/* the bridge verbs the doors read through, given by the app that owns the
   client adapter: the inventory on disk and one plugin's module */
export interface UxDoorApi {
  list: () => Promise<{ plugins: UxPluginListing[] }>;
  source: (id: string) => Promise<UxPluginSource>;
}

let doors: UxDoorApi | null = null;

export function configureUxDoors(api: UxDoorApi): void {
  doors = api;
}

function doorApi(): UxDoorApi {
  if (!doors) throw new Error("UX doors are not configured: no client adapter");
  return doors;
}

export async function listRuntimeUxPlugins(): Promise<UxPluginListing[]> {
  const { plugins } = await doorApi().list();
  return plugins;
}

/* the inventory row for an entry whose source never arrived: published as
   a row, not evaluated as a module */
function publishUnreadable(entry: UxPluginListing, error: unknown): void {
  publishUxPlugin({
    id: entry.id,
    name: entry.id,
    door: entry.door,
    file: entry.file,
    pack: entry.pack ?? undefined,
    status: "error",
    error: error instanceof Error ? error.message : String(error),
  });
}

/** The plugin id a listing declares: the manifest's `name` for a pack
    plugin (`<ns>/<pack>/<name>`), the folder name for a dev one. Decisions
    are keyed on it, so enablement is known before any source is read, and
    the module's own `id` must agree. */
export function declaredPluginId(entry: UxPluginListing): string {
  return entry.door === "pack"
    ? entry.id.slice(entry.id.lastIndexOf("/") + 1)
    : entry.id;
}

function enabledByListing(entry: UxPluginListing): boolean {
  return uxPluginActive(declaredPluginId(entry), listingDefaultEnabled(entry));
}

function listingDefaultEnabled(entry: UxPluginListing): boolean {
  return entry.door === "dev" ? false : (entry.defaultEnabled ?? true);
}

/* who holds a plugin id: a listing, or the bundled copy the loader shadows
   runtime copies behind */
function ownerOf(id: string): string | undefined {
  const bundled = uxPluginRecords()[id];
  if (bundled?.door === "bundled") return "bundled";
  return owners.get(id);
}

function claim(entry: UxPluginListing): string | null {
  const id = declaredPluginId(entry);
  const owner = ownerOf(id);
  if (owner && owner !== entry.id) return owner;
  owners.set(id, entry.id);
  return null;
}

function release(listingId: string): void {
  for (const [id, owner] of owners) {
    if (owner === listingId) owners.delete(id);
  }
}

const shadowedId = (entry: UxPluginListing) => `${entry.id}:shadowed`;

/* the row of a listing whose plugin id another listing owns: visible,
   off, and refusing to load until the owner is gone */
function publishShadowed(entry: UxPluginListing, owner: string): void {
  const id = declaredPluginId(entry);
  const error = `duplicate id "${id}", owned by ${owner}; this copy is not used`;
  const row = {
    id: shadowedId(entry),
    name: `${id} (shadowed)`,
    description: error,
    door: entry.door,
    file: entry.file,
    producer: entry.producer,
    pack: entry.pack ?? undefined,
    declared: entry.contributes ?? undefined,
  };
  publishUxPlugin(
    { ...row, status: "disabled" },
    {
      activate: () => publishUxPlugin({ ...row, status: "error", error }),
      deactivate: () => publishUxPlugin({ ...row, status: "disabled" }),
    },
  );
  known.set(entry.id, null);
}

/* the inventory row of a plugin that is off: built from the listing alone,
   its producer never run and its module never evaluated; the switch loads it */
function publishListed(entry: UxPluginListing): void {
  const id = declaredPluginId(entry);
  const owner = claim(entry);
  if (owner) {
    publishShadowed(entry, owner);
    return;
  }
  publishUxPlugin(
    {
      id,
      name: id,
      description: entry.description ?? undefined,
      door: entry.door,
      file: entry.file,
      producer: entry.producer,
      pack: entry.pack ?? undefined,
      declared: entry.contributes ?? undefined,
      status: "disabled",
    },
    {
      activate: () => loadListing(entry),
      deactivate: () => undefined,
    },
  );
  known.set(entry.id, id);
}

async function loadListing(entry: UxPluginListing): Promise<void> {
  const previous = known.get(entry.id) ?? null;
  const owner = claim(entry);
  if (owner) {
    if (previous) retireRuntimeUxPlugin(previous);
    publishShadowed(entry, owner);
    return;
  }
  let module: UxPluginSource | null = null;
  let unreadable: unknown = null;
  try {
    module = await doorApi().source(entry.id);
  } catch (error) {
    unreadable = error;
  }
  /* the previous incarnation goes before anything is published for this
     entry: a plugin that no longer loads must not stay registered beside
     its error row, and when its id is the entry's own, that row must not
     be swept away with it */
  if (previous) retireRuntimeUxPlugin(previous);
  if (!module) {
    publishUnreadable(entry, unreadable);
    known.set(entry.id, null);
    return;
  }
  const id = await loadRuntimeUxPlugin(
    { source: module.source, css: module.css },
    {
      origin: entry.id,
      expectId: declaredPluginId(entry),
      door: entry.door,
      file: entry.file,
      producer: entry.producer,
      pack: entry.pack ?? undefined,
      listing: entry.pack ? entry.id : undefined,
      declared: entry.contributes ?? undefined,
      defaultEnabled: listingDefaultEnabled(entry),
    },
  );
  known.set(entry.id, id);
}

/* reconcile disk with the inventory; `reloadKnown` re-reads every entry,
   a plain scan re-reads only what is new or broken */
async function reconcile(reloadKnown: boolean): Promise<void> {
  let listings: UxPluginListing[];
  try {
    listings = await listRuntimeUxPlugins();
  } catch {
    /* no local agent home (mobile, or not started): nothing to scan */
    return;
  }
  const seen = new Set(listings.map((entry) => entry.id));
  /* what left the disk goes first, so an id it owned is free for a listing
     that was shadowed behind it in this same scan */
  for (const [id, loaded] of known) {
    if (seen.has(id)) continue;
    /* only the incarnation this listing owns leaves with it: a shadowed
       copy's owner keeps running */
    if (loaded && owners.get(loaded) === id) retireRuntimeUxPlugin(loaded);
    for (const row of [id, `${id}:shadowed`]) {
      if (uxPluginRecords()[row]) retireRuntimeUxPlugin(row);
    }
    release(id);
    known.delete(id);
  }
  for (const entry of listings.sort((a, b) => a.id.localeCompare(b.id))) {
    if (known.get(entry.id) && !reloadKnown) continue;
    if (!enabledByListing(entry)) {
      /* off: the row says so from the manifest; nothing of it runs */
      const previous = known.get(entry.id) ?? null;
      if (previous) retireRuntimeUxPlugin(previous);
      publishListed(entry);
      continue;
    }
    await loadListing(entry);
  }
}

/** scan the doors; a scan asked for while one runs follows it, never drops */
export function scanRuntimeUxPlugins(reloadKnown = false): Promise<void> {
  const run = () => reconcile(reloadKnown);
  const scan: Promise<void> = (inFlight ?? Promise.resolve()).then(run, run);
  inFlight = scan;
  void scan.finally(() => {
    if (inFlight === scan) inFlight = null;
  });
  return scan;
}

/** the panel's Reload button */
export const reloadRuntimeUxPlugins = (): Promise<void> => scanRuntimeUxPlugins(true);

/** test seam */
export function resetRuntimeDoor(): void {
  known.clear();
  owners.clear();
  inFlight = null;
}

/* The runtime doors from the webview: ask the bridge what is on disk
   (`<home>/ux-plugins/<id>/` and every installed pack's ux[]) and which
   page documents the selected node holds, fetch each module's source, and
   push it through the loader. A manual Reload re-reads every entry (an
   installer that replaces a folder atomically keeps the same path); a page
   document is re-read whenever its listed digest changes, so an edit on the
   node reaches the app without a reinstall. Rust owns the scan; this owns
   the lifecycle. */
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
/* page document listing id -> the content digest it was last loaded or
   listed at; a different digest in a later scan is an edit */
const digests = new Map<string, string | null>();
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
  return entry.door === "dev"
    ? entry.id
    : entry.id.slice(entry.id.lastIndexOf("/") + 1);
}

/* a page document that is disabled never loads, whatever this window chose
   earlier: the document decides, and the node refuses its data anyway */
const enabledByDocument = (entry: UxPluginListing) =>
  !(entry.door === "document" && entry.defaultEnabled === false);

function enabledByListing(entry: UxPluginListing): boolean {
  if (!enabledByDocument(entry)) return false;
  return uxPluginActive(declaredPluginId(entry), listingDefaultEnabled(entry));
}

/* whether a page document differs from what this window holds: its content
   was edited, or it was enabled or disabled since it was loaded or listed */
function documentChanged(entry: UxPluginListing): boolean {
  if (entry.door !== "document" || !digests.has(entry.id)) return false;
  const loaded = uxPluginRecords()[known.get(entry.id) ?? ""]?.status === "loaded";
  return (
    digests.get(entry.id) !== (entry.digest ?? null) ||
    enabledByListing(entry) !== loaded
  );
}

function noteDigest(entry: UxPluginListing): void {
  if (entry.door === "document") digests.set(entry.id, entry.digest ?? null);
}

/* a pack page and a page document carry grants: the frame is told its
   listing so its data calls are checked against that declaration */
const carriesGrants = (entry: UxPluginListing) =>
  Boolean(entry.pack) || entry.door === "document";

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
      /* the switch cannot turn on a page its document turned off */
      activate: () =>
        enabledByDocument(entry)
          ? loadListing(entry)
          : publishUxPlugin({
              id,
              name: id,
              description: "Disabled on its node; enable the page document there.",
              door: entry.door,
              file: entry.file,
              declared: entry.contributes ?? undefined,
              status: "disabled",
            }),
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
      listing: carriesGrants(entry) ? entry.id : undefined,
      declared: entry.contributes ?? undefined,
      defaultEnabled: listingDefaultEnabled(entry),
    },
  );
  known.set(entry.id, id);
}

/* reconcile disk with the inventory; `reloadKnown` re-reads every entry,
   a plain scan re-reads only what is new or broken */
async function reconcile(reloadKnown: boolean, reloadPack?: string): Promise<void> {
  let listings: UxPluginListing[];
  try {
    listings = await listRuntimeUxPlugins();
  } catch (error) {
    if (reloadPack !== undefined) throw error;
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
    digests.delete(id);
  }
  for (const entry of listings.sort((a, b) => a.id.localeCompare(b.id))) {
    if (
      known.get(entry.id) &&
      !reloadKnown &&
      !documentChanged(entry) &&
      (reloadPack === undefined || entry.pack !== reloadPack)
    )
      continue;
    noteDigest(entry);
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
function queueScan(reloadKnown: boolean, reloadPack?: string): Promise<void> {
  const run = () => reconcile(reloadKnown, reloadPack);
  const scan: Promise<void> = (inFlight ?? Promise.resolve()).then(run, run);
  inFlight = scan;
  const clear = () => {
    if (inFlight === scan) inFlight = null;
  };
  void scan.then(clear, clear);
  return scan;
}

export function scanRuntimeUxPlugins(reloadKnown = false): Promise<void> {
  return queueScan(reloadKnown);
}

/* A page document changes on its node, not in this window, so nothing here
   announces the edit. The app asks for a scan when the selected node
   changes, and on arrival and client activity while a screen shows pages;
   this coalesces those into one scan per interval. A scan that finds the same digests reads no source. */
const RESCAN_AFTER_MS = 2000;
let rescan: ReturnType<typeof setTimeout> | null = null;

export function rescanRuntimeUxPluginsSoon(): void {
  if (rescan !== null) return;
  rescan = setTimeout(() => {
    rescan = null;
    void scanRuntimeUxPlugins().catch(() => undefined);
  }, RESCAN_AFTER_MS);
}

const CHANGED_KEY = "gents.ux-plugins-changed.v1";

/** after a pack changes: reloads its pages here and in the app's other
    windows; rejects when this window's reload fails */
export function refreshRuntimePackUxPlugins(pack: string): Promise<void> {
  try {
    window.localStorage.setItem(CHANGED_KEY, JSON.stringify({ pack, at: Date.now() }));
  } catch {
    /* no storage: only this window reloads */
  }
  return queueScan(false, pack);
}

/** the panel's Reload button */
export const reloadRuntimeUxPlugins = (): Promise<void> => scanRuntimeUxPlugins(true);

/** settles once no scan is running, so a plugin missing mid-reload is not
    taken for gone */
export function uxScanSettled(): Promise<void> {
  return inFlight ?? Promise.resolve();
}

/* Every window keeps its own inventory, so a pack installed, updated or
   removed in one is reloaded in all: here at once, the others through a
   storage event (which never reaches the window that wrote it). */
export function followOtherWindowsPackChanges(): () => void {
  const follow = (event: StorageEvent) => {
    if (event.key !== CHANGED_KEY || !event.newValue) return;
    let pack: unknown;
    try {
      pack = (JSON.parse(event.newValue) as { pack?: unknown }).pack;
    } catch {
      /* unreadable: a plain scan */
    }
    /* the window that made the change reports a failed reload */
    const scan = typeof pack === "string" ? queueScan(false, pack) : queueScan(false);
    void scan.catch(() => undefined);
  };
  window.addEventListener("storage", follow);
  return () => window.removeEventListener("storage", follow);
}

/** test seam */
export function resetRuntimeDoor(): void {
  known.clear();
  owners.clear();
  digests.clear();
  if (rescan !== null) clearTimeout(rescan);
  rescan = null;
  inFlight = null;
}

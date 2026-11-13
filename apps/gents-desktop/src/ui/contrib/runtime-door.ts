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
import {
  publishUxPlugin,
  uxDecisionKey,
  uxPluginActive,
  uxPluginRecords,
} from "./plugins-store";

/* the listing and the source as the client's adapter types them; the door
   narrows nothing further */
export type { UxPluginListing, UxPluginSource };

/* A listing as one scan read it. A page document's id (`page/<page_id>`)
   names it only within its node, so everything this window keeps about one
   (what is loaded, its digest, the person's choice, its storage) is kept
   under `key`, which carries the node; a home listing's key is its id. The
   node and its epoch are the ones the scan captured, so the module is read
   from the node that listed it and is dropped if that node was left. */
interface Scanned extends UxPluginListing {
  key: string;
  node: string | null;
  epoch: number;
}

interface Known {
  /* the listing id, which the entry's error and shadowed rows are named by */
  listing: string;
  /* the loaded plugin id; null while broken or shadowed */
  loaded: string | null;
  document: boolean;
}

/* live runtime plugins by listing key */
const known = new Map<string, Known>();
/* plugin id -> the listing key that owns it. Two packs may ship a plugin of
   one name; the first listing (scan order) owns the id whatever its state,
   and the other is shown as shadowed until the owner leaves. */
const owners = new Map<string, string>();
/* page document listing key -> the content digest it was last loaded or
   listed at; a different digest in a later scan is an edit */
const digests = new Map<string, string | null>();
/* the scan in flight; a scan asked for meanwhile runs after it */
let inFlight: Promise<void> | null = null;
/* the node whose page documents this window holds, and a count of the times
   it changed: a scan that began under an earlier count holds another node's
   pages, even when the selection has since come back to that node */
let pageNode: string | null = null;
let nodeEpoch = 0;

/* the bridge verbs the doors read through, given by the app that owns the
   client adapter: the inventory on disk and one plugin's module, each read
   for the node the door names, and the node selected now */
export interface UxDoorApi {
  list: (
    nodeDid: string | null,
  ) => Promise<{ plugins: UxPluginListing[]; pagesError?: string | null }>;
  source: (id: string, nodeDid: string | null) => Promise<UxPluginSource>;
  node?: () => string | null;
}

let doors: UxDoorApi | null = null;

export function configureUxDoors(api: UxDoorApi): void {
  doors = api;
}

function doorApi(): UxDoorApi {
  if (!doors) throw new Error("UX doors are not configured: no client adapter");
  return doors;
}

const selectedNode = (): string | null => doorApi().node?.() ?? null;

const isDocument = (entry: UxPluginListing) => entry.door === "document";

function scanned(entry: UxPluginListing, node: string | null, epoch: number): Scanned {
  const key = isDocument(entry) ? `${node ?? ""}/${entry.id}` : entry.id;
  return { ...entry, key, node, epoch };
}

/* what a page document's choice and storage are kept under; a home plugin
   keeps its own id */
const scopeOf = (entry: Scanned) => (isDocument(entry) ? entry.key : undefined);

/* a page document is current only while the node it was listed on has not
   been left since */
const current = (entry: Scanned) => !isDocument(entry) || entry.epoch === nodeEpoch;

/* the row that says a node's page documents could not be read. No listing
   id is `page/` (a page id is never empty), so it shadows no entry's row. */
const PAGES_ROW = "page/";

function showPagesError(error: string | null): void {
  if (error === null) {
    if (uxPluginRecords()[PAGES_ROW]) retireRuntimeUxPlugin(PAGES_ROW);
    return;
  }
  publishUxPlugin({
    id: PAGES_ROW,
    name: "Pages on this node",
    door: "document",
    status: "error",
    error,
  });
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

function enabledByListing(entry: Scanned): boolean {
  if (!enabledByDocument(entry)) return false;
  return uxPluginActive(
    uxDecisionKey({ id: declaredPluginId(entry), scope: scopeOf(entry) }),
    listingDefaultEnabled(entry),
  );
}

/* a pack reinstalled outside this window (the CLI) announces nothing here;
   its listing digest is how a scan learns the plugin's code changed */
const tracksDigest = (entry: Scanned) =>
  isDocument(entry) || (entry.door === "pack" && entry.digest != null);

/* whether a listing differs from what this window holds: a page document or
   pack plugin was edited, or a page document was enabled or disabled since it
   was loaded or listed */
function contentChanged(entry: Scanned): boolean {
  if (!tracksDigest(entry) || !digests.has(entry.key)) return false;
  if (!isDocument(entry)) return digests.get(entry.key) !== (entry.digest ?? null);
  const loaded =
    uxPluginRecords()[known.get(entry.key)?.loaded ?? ""]?.status === "loaded";
  return (
    digests.get(entry.key) !== (entry.digest ?? null) ||
    enabledByListing(entry) !== loaded
  );
}

function noteDigest(entry: Scanned): void {
  if (tracksDigest(entry)) digests.set(entry.key, entry.digest ?? null);
}

function note(entry: Scanned, loaded: string | null): void {
  known.set(entry.key, { listing: entry.id, loaded, document: isDocument(entry) });
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

function claim(entry: Scanned): string | null {
  const id = declaredPluginId(entry);
  const owner = ownerOf(id);
  if (owner && owner !== entry.key) return owner;
  owners.set(id, entry.key);
  return null;
}

function release(key: string): void {
  for (const [id, owner] of owners) {
    if (owner === key) owners.delete(id);
  }
}

/* everything one listing put in this window: its plugin (only the
   incarnation it owns; a shadowed copy's owner keeps running), its error
   and shadowed rows, its claim and its digest */
function retireListing(key: string, entry: Known): void {
  if (entry.loaded && owners.get(entry.loaded) === key) {
    retireRuntimeUxPlugin(entry.loaded);
  }
  for (const row of [entry.listing, `${entry.listing}:shadowed`]) {
    if (uxPluginRecords()[row]) retireRuntimeUxPlugin(row);
  }
  release(key);
  known.delete(key);
  digests.delete(key);
}

/* The selection left a node: its pages stop at once, not when the next
   node's scan gets to them. A frame left running would make its data calls
   against the node now selected, under grants its own node gave it. */
function leaveNode(next: string | null): void {
  nodeEpoch += 1;
  pageNode = next;
  for (const [key, entry] of known) {
    if (entry.document) retireListing(key, entry);
  }
  showPagesError(null);
}

const shadowedId = (entry: UxPluginListing) => `${entry.id}:shadowed`;

/* the row of a listing whose plugin id another listing owns: visible,
   off, and refusing to load until the owner is gone */
function publishShadowed(entry: Scanned, owner: string): void {
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
  note(entry, null);
}

/* the inventory row of a plugin that is off: built from the listing alone,
   its producer never run and its module never evaluated; the switch loads it */
function publishListed(entry: Scanned): void {
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
      scope: scopeOf(entry),
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
              scope: scopeOf(entry),
              status: "disabled",
            }),
      deactivate: () => undefined,
    },
  );
  note(entry, id);
}

async function loadListing(entry: Scanned): Promise<void> {
  const previous = known.get(entry.key)?.loaded ?? null;
  const owner = claim(entry);
  if (owner) {
    if (previous) retireRuntimeUxPlugin(previous);
    publishShadowed(entry, owner);
    return;
  }
  let module: UxPluginSource | null = null;
  let unreadable: unknown = null;
  try {
    module = await doorApi().source(entry.id, entry.node);
  } catch (error) {
    unreadable = error;
  }
  /* its node was left while the module was read: leaving retired what the
     page had here, and the module belongs to no page of the node selected */
  if (!current(entry)) {
    release(entry.key);
    return;
  }
  /* the previous incarnation goes before anything is published for this
     entry: a plugin that no longer loads must not stay registered beside
     its error row, and when its id is the entry's own, that row must not
     be swept away with it */
  if (previous) retireRuntimeUxPlugin(previous);
  if (!module) {
    publishUnreadable(entry, unreadable);
    note(entry, null);
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
      scope: scopeOf(entry),
    },
  );
  /* left while its frame was loading: the frame goes, or the row its
     failure was published on */
  if (!current(entry)) {
    retireRuntimeUxPlugin(id ?? entry.id);
    release(entry.key);
    return;
  }
  note(entry, id);
}

/* reconcile disk and the selected node with the inventory; `reloadKnown`
   re-reads every entry, a plain scan re-reads only what is new or broken.
   The node is read once: the listing and every module of this scan come
   from it, whatever the selection does meanwhile. */
async function reconcile(reloadKnown: boolean, reloadPack?: string): Promise<void> {
  const node = selectedNode();
  if (node !== pageNode) leaveNode(node);
  const epoch = nodeEpoch;
  let answer: Awaited<ReturnType<UxDoorApi["list"]>>;
  try {
    answer = await doorApi().list(node);
  } catch (error) {
    if (reloadPack !== undefined) throw error;
    /* no local agent home (mobile, or not started): nothing to scan */
    return;
  }
  /* the node was left while it was listed: its pages are not wanted, and
     the scan that leaving queued reads the node selected now */
  const left = epoch !== nodeEpoch;
  /* a node whose pages could not be read listed none; that is not the
     pages being deleted, so the ones loaded stay and the row says why */
  const pagesError = left ? null : (answer.pagesError ?? null);
  if (!left) showPagesError(pagesError);
  const listings = answer.plugins
    .filter((entry) => !(left && isDocument(entry)))
    .map((entry) => scanned(entry, node, epoch));
  const seen = new Set(listings.map((entry) => entry.key));
  /* what left the disk goes first, so an id it owned is free for a listing
     that was shadowed behind it in this same scan */
  for (const [key, entry] of known) {
    if (seen.has(key) || (entry.document && pagesError !== null)) continue;
    retireListing(key, entry);
  }
  for (const entry of listings.sort((a, b) => a.id.localeCompare(b.id))) {
    if (!current(entry)) continue;
    if (
      known.get(entry.key)?.loaded &&
      !reloadKnown &&
      !contentChanged(entry) &&
      (reloadPack === undefined || entry.pack !== reloadPack)
    )
      continue;
    noteDigest(entry);
    if (!enabledByListing(entry)) {
      /* off: the row says so from the manifest; nothing of it runs */
      const previous = known.get(entry.key)?.loaded ?? null;
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

/** the selected node may have changed: if it did, the pages of the node
    left are retired before this returns, and a scan reads the new one's */
export function followSelectedUxNode(): Promise<void> {
  const node = selectedNode();
  if (node === pageNode) return uxScanSettled();
  leaveNode(node);
  return queueScan(false);
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
  pageNode = null;
  nodeEpoch = 0;
  if (rescan !== null) clearTimeout(rescan);
  rescan = null;
  inFlight = null;
}

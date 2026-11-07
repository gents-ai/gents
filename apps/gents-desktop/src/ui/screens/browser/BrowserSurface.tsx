/* A browser in the panel. The page is a native view beneath the app
   (browser.rs); this surface draws its toolbar and holds the page's place,
   which seeThrough keeps open to the page. */
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import {
  ArrowLeft,
  ArrowRight,
  FolderOpen,
  Globe,
  RotateCw,
  Search,
  X,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { inNativeShell } from "../../../lib/nativeShell";
import type { SurfaceContext } from "@/app/surfaces";
import { keyEventOf, type HandedKey } from "./handedKeys";
import { nextSeq } from "./order";
import { seeThrough } from "./seeThrough";

const RECENTS_KEY = "gents-browser-recents";

type Recent = { url: string; title: string };

function readRecents(): Recent[] {
  try {
    return JSON.parse(localStorage.getItem(RECENTS_KEY) ?? "[]") as Recent[];
  } catch {
    return [];
  }
}

function remember(url: string, title: string) {
  try {
    const next = [
      { url, title: title || new URL(url).host },
      ...readRecents().filter((r) => r.url !== url),
    ].slice(0, 8);
    localStorage.setItem(RECENTS_KEY, JSON.stringify(next));
  } catch {
    /* recents are a convenience */
  }
}

/* an address, or else a search */
function destination(text: string) {
  const value = text.trim();
  if (!value) return null;
  if (/^https?:\/\//i.test(value)) return value;
  /* a file on this machine: the native side resolves and checks it */
  if (/^file:\/\//i.test(value) || value.startsWith("/") || value.startsWith("~/"))
    return value;
  if (/^localhost(:\d+)?(\/.*)?$/.test(value)) return `http://${value}`;
  if (/^[\w-]+(\.[\w-]+)+(:\d+)?(\/.*)?$/.test(value)) return `https://${value}`;
  return `https://duckduckgo.com/?q=${encodeURIComponent(value)}`;
}

/* the page the native view holds; it outlives this surface, which unmounts
   whenever another panel tab is chosen */
type Page = {
  opened: boolean;
  url: string;
  title: string;
  loading: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
};

let page: Page = {
  opened: false,
  url: "",
  title: "",
  loading: false,
  canGoBack: false,
  canGoForward: false,
};
const listeners = new Set<() => void>();
let following = false;

/* the shown surface's address field, for ⌘L from the page */
let focusAddress: (() => void) | null = null;

/* A command key from the page: the browser's own (⌘L the address, ⌘R
   reload, ⌘[ and ⌘] back and forward), or else replayed to the app's
   shortcuts, which now have the keyboard back. */
function handOver(handed: HandedKey) {
  const init = keyEventOf(handed);
  const plain = !handed.shift && !handed.alt && !handed.ctrl;
  const own: Record<string, () => void> = {
    KeyL: () => focusAddress?.(),
    KeyR: () => void invoke("browser_go", { action: "reload" }).catch(() => {}),
    BracketLeft: () => void invoke("browser_go", { action: "back" }).catch(() => {}),
    BracketRight: () =>
      void invoke("browser_go", { action: "forward" }).catch(() => {}),
  };
  const mine = plain ? own[init.code ?? ""] : undefined;
  if (mine) return mine();
  document.body.dispatchEvent(new KeyboardEvent("keydown", init));
}

function follow() {
  if (following || !inNativeShell()) return;
  following = true;
  void listen<Omit<Page, "opened">>("browser-state", ({ payload }) => {
    const finished = page.loading && !payload.loading;
    page = { ...page, ...payload };
    if (finished && payload.url) remember(payload.url, payload.title);
    for (const notify of listeners) notify();
  });
  void listen<HandedKey>("browser-key", ({ payload }) => handOver(payload));
}

/** Drops a page a previous load of this window left behind: the native view
    outlives a reload of the app's page, which starts with none open. */
export function resetBrowser() {
  void invoke("browser_close", { seq: nextSeq() }).catch(() => {});
  follow();
}

const subscribe = (notify: () => void) => {
  listeners.add(notify);
  return () => {
    listeners.delete(notify);
  };
};

function open(url: string) {
  page = { ...page, opened: true, url, loading: true };
  for (const notify of listeners) notify();
  void invoke("browser_open", { url, seq: nextSeq() }).catch(() => {});
}

/* a page from this machine, picked in the system's own dialog */
async function openFile() {
  const picked = await openDialog({ multiple: false, directory: false }).catch(
    () => null,
  );
  if (typeof picked === "string") open(picked);
}

function closePage() {
  page = {
    opened: false,
    url: "",
    title: "",
    loading: false,
    canGoBack: false,
    canGoForward: false,
  };
  for (const notify of listeners) notify();
  void invoke("browser_close", { seq: nextSeq() }).catch(() => {});
}

export function BrowserSurface(_: SurfaceContext) {
  const current = useSyncExternalStore(subscribe, () => page);
  const [address, setAddress] = useState(current.url);
  const [editing, setEditing] = useState(false);
  const [recents, setRecents] = useState(readRecents);
  const place = useRef<HTMLDivElement>(null);
  const addressField = useRef<HTMLInputElement>(null);

  useEffect(() => {
    follow();
    focusAddress = () => addressField.current?.focus();
    return () => {
      focusAddress = null;
    };
  }, []);

  /* the address follows the page unless it's being typed into */
  useEffect(() => {
    if (!editing) setAddress(current.url);
  }, [current.url, editing]);

  useEffect(() => {
    if (!current.loading) setRecents(readRecents());
  }, [current.loading]);

  useEffect(() => {
    const el = place.current;
    if (!current.opened || !el || !inNativeShell()) return;
    return seeThrough(el);
  }, [current.opened]);

  const go = (text: string) => {
    const next = destination(text);
    if (!next) return;
    setEditing(false);
    setAddress(next);
    open(next);
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-0.5 border-b border-border/60 px-1.5">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Back"
          disabled={!current.canGoBack}
          onClick={() => void invoke("browser_go", { action: "back" })}
        >
          <ArrowLeft />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Forward"
          disabled={!current.canGoForward}
          onClick={() => void invoke("browser_go", { action: "forward" })}
        >
          <ArrowRight />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={current.loading ? "Stop" : "Reload"}
          disabled={!current.opened}
          onClick={() =>
            void invoke("browser_go", { action: current.loading ? "stop" : "reload" })
          }
        >
          {current.loading ? <X /> : <RotateCw />}
        </Button>
        <form
          className="min-w-0 flex-1"
          onSubmit={(e) => {
            e.preventDefault();
            go(address);
          }}
        >
          <Input
            ref={addressField}
            aria-label="Address"
            value={address}
            onFocus={(e) => {
              setEditing(true);
              e.currentTarget.select();
            }}
            onBlur={() => setEditing(false)}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="Search or enter address"
            className="h-6 rounded-md border-transparent bg-transparent px-1.5 text-xs shadow-none hover:bg-muted/50 focus-visible:bg-muted dark:bg-transparent"
          />
        </form>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Open a file"
          onClick={() => void openFile()}
        >
          <FolderOpen />
        </Button>
        {current.opened && (
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Close page"
            onClick={closePage}
          >
            <X />
          </Button>
        )}
      </div>
      {current.opened ? (
        <div ref={place} data-testid="browser-view" className="min-h-0 flex-1" />
      ) : (
        <div className="mx-auto w-full max-w-md px-4 pt-16">
          <form
            onSubmit={(e) => {
              e.preventDefault();
              go(address);
            }}
            className="relative"
          >
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              autoFocus
              aria-label="Search or enter address"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              placeholder="Search, or enter an address or a file path…"
              className="h-8 pl-8 text-xs"
            />
          </form>
          <button
            type="button"
            onClick={() => void openFile()}
            className="mt-3 flex cursor-pointer items-center gap-1.5 px-1 text-xs text-muted-foreground hover:text-foreground"
          >
            <FolderOpen className="size-3.5" />
            Open a file on this Mac…
          </button>
          {recents.length > 0 && (
            <div className="mt-6">
              <p className="mb-2 text-xs text-muted-foreground">Recent</p>
              <ul className="space-y-1">
                {recents.map((r) => (
                  <li key={r.url}>
                    <button
                      type="button"
                      onClick={() => go(r.url)}
                      className="flex w-full cursor-pointer items-center gap-2 rounded px-1 py-1 text-left text-sm hover:bg-muted"
                    >
                      <Globe className="size-3.5 shrink-0 text-muted-foreground" />
                      <span className="truncate">{r.title}</span>
                      <span className="truncate text-xs text-muted-foreground">
                        {r.url}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {!inNativeShell() && (
            <p className="mt-6 text-xs text-muted-foreground">
              The browser needs the desktop app.
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/* A browser in the panel. Each tab's page is a native view beneath the app
   (browser.rs); this surface draws the tabs and the toolbar and holds the
   page's place, which seeThrough keeps open to the page. */
import { useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  ArrowRight,
  FolderOpen,
  Globe,
  Plus,
  RotateCw,
  Search,
  X,
} from "lucide-react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { cn } from "@gents/ui/lib/utils";
import { inNativeShell } from "../../../lib/nativeShell";
import type { SurfaceContext } from "@/app/surfaces";
import { seeThrough } from "./seeThrough";
import {
  activeTab,
  closeTab,
  follow,
  go,
  newTab,
  onFocusAddress,
  open,
  readRecents,
  selectTab,
  useBrowser,
  type Tab,
} from "./tabs";

export { resetBrowser } from "./tabs";

/* an address, a file on this machine, or else a search */
function destination(text: string) {
  const value = text.trim();
  if (!value) return null;
  if (/^https?:\/\//i.test(value)) return value;
  /* the native side resolves and checks a file */
  if (/^file:\/\//i.test(value) || value.startsWith("/") || value.startsWith("~/"))
    return value;
  if (/^localhost(:\d+)?(\/.*)?$/.test(value)) return `http://${value}`;
  if (/^[\w-]+(\.[\w-]+)+(:\d+)?(\/.*)?$/.test(value)) return `https://${value}`;
  return `https://duckduckgo.com/?q=${encodeURIComponent(value)}`;
}

/* a page from this machine, picked in the system's own dialog */
async function openFile() {
  const picked = await openDialog({ multiple: false, directory: false }).catch(
    () => null,
  );
  if (typeof picked === "string") open(picked);
}

function tabName(tab: Tab) {
  if (tab.title) return tab.title;
  if (!tab.opened) return "New tab";
  try {
    const url = new URL(tab.url);
    return url.protocol === "file:"
      ? decodeURIComponent(url.pathname.split("/").pop() ?? "")
      : url.host;
  } catch {
    return tab.url;
  }
}

function Tabs({ tabs, active }: { tabs: Tab[]; active: string }) {
  return (
    <div
      role="tablist"
      aria-label="Browser tabs"
      className="flex h-8 shrink-0 items-center gap-0.5 overflow-x-auto border-b border-border/60 px-1.5"
    >
      {tabs.map((tab) => {
        const current = tab.id === active;
        return (
          <div
            key={tab.id}
            className={cn(
              "group flex h-6 max-w-44 min-w-0 shrink-0 items-center rounded-md text-xs",
              current
                ? "bg-muted text-foreground"
                : "text-muted-foreground hover:bg-muted/50",
            )}
          >
            <button
              type="button"
              role="tab"
              aria-selected={current}
              onClick={() => selectTab(tab.id)}
              className="min-w-0 flex-1 cursor-pointer truncate py-0.5 pr-1 pl-2 text-left"
            >
              {tabName(tab)}
            </button>
            <button
              type="button"
              aria-label={`Close ${tabName(tab)}`}
              onClick={() => closeTab(tab.id)}
              className={cn(
                "mr-0.5 grid size-5 shrink-0 cursor-pointer place-items-center rounded hover:bg-muted",
                !current &&
                  "opacity-0 group-hover:opacity-100 focus-visible:opacity-100",
              )}
            >
              <X className="size-3" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

export function BrowserSurface(_: SurfaceContext) {
  const browser = useBrowser();
  const current = activeTab(browser);
  const [address, setAddress] = useState(current.url);
  const [editing, setEditing] = useState(false);
  const [recents, setRecents] = useState(readRecents);
  const place = useRef<HTMLDivElement>(null);
  const addressField = useRef<HTMLInputElement>(null);
  const startField = useRef<HTMLInputElement>(null);

  useEffect(() => {
    follow();
    onFocusAddress(() => (startField.current ?? addressField.current)?.focus());
    return () => onFocusAddress(null);
  }, []);

  /* the address follows the tab and its page unless it's being typed into */
  useEffect(() => {
    if (!editing) setAddress(current.url);
  }, [current.id, current.url, editing]);

  useEffect(() => {
    if (!current.loading) setRecents(readRecents());
  }, [current.loading]);

  useEffect(() => {
    const el = place.current;
    if (!current.opened || !el || !inNativeShell()) return;
    return seeThrough(el);
  }, [current.opened]);

  const submit = (text: string) => {
    const next = destination(text);
    if (!next) return;
    setEditing(false);
    setAddress(next);
    open(next);
  };

  return (
    <div className="flex h-full min-h-0 flex-col">
      {browser.tabs.length > 1 && <Tabs tabs={browser.tabs} active={browser.active} />}
      <div className="flex h-9 shrink-0 items-center gap-0.5 border-b border-border/60 px-1.5">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Back"
          disabled={!current.canGoBack}
          onClick={() => go("back")}
        >
          <ArrowLeft />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Forward"
          disabled={!current.canGoForward}
          onClick={() => go("forward")}
        >
          <ArrowRight />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={current.loading ? "Stop" : "Reload"}
          disabled={!current.opened}
          onClick={() => go(current.loading ? "stop" : "reload")}
        >
          {current.loading ? <X /> : <RotateCw />}
        </Button>
        <form
          className="min-w-0 flex-1"
          onSubmit={(e) => {
            e.preventDefault();
            submit(address);
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
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="New tab"
          onClick={() => newTab()}
        >
          <Plus />
        </Button>
        {browser.tabs.length === 1 && current.opened && (
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Close page"
            onClick={() => closeTab(current.id)}
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
              submit(address);
            }}
            className="relative"
          >
            <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              ref={startField}
              autoFocus
              aria-label="Search or enter address"
              value={address}
              onChange={(e) => setAddress(e.target.value)}
              placeholder="Search, or enter an address or a file path…"
              className="h-8 pl-8 text-xs"
            />
          </form>
          {current.error && (
            <p role="alert" className="mt-2 px-1 text-xs text-destructive">
              {current.error}
            </p>
          )}
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
                      onClick={() => submit(r.url)}
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

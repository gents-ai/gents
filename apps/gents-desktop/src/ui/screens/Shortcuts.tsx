/* Keyboard shortcuts: ⌘1 agents, ⌘2 sessions, ⌘3 configuration,
   ⌘N new session, ⌘K focus the composer, ⌘/ this sheet. */
import { useEffect, useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { Kbd } from "@gents/ui/components/kbd";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";

const IS_MAC = navigator.platform.toUpperCase().includes("MAC");
const MOD = IS_MAC ? "⌘" : "Ctrl+";
const IN_DESKTOP = "__TAURI_INTERNALS__" in window || "__TAURI__" in window;
const DESKTOP_ONLY = new Set(["1", "2", "3", "n"]);
const ROWS: [string, string][] = [
  [`${MOD}1`, "Agents"],
  [`${MOD}2`, "Sessions"],
  [`${MOD}3`, "Configuration"],
  [`${MOD}N`, "New session"],
  [`${MOD}K`, "Focus the composer"],
  [`${MOD}/`, "Show this reference"],
];

export function Shortcuts({ shell }: { shell: Shell }) {
  const [open, setOpen] = useState(false);
  const agentDid = shell.selectedAgentDid;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey || e.shiftKey) return;
      if (!IN_DESKTOP && DESKTOP_ONLY.has(e.key)) return;
      const go = (to: Parameters<typeof navigate>[0]) => {
        e.preventDefault();
        navigate(to);
      };
      switch (e.key) {
        case "1":
          return go({ name: "agents" });
        case "2":
          return go({ name: "sessions" });
        case "3":
          return go(
            agentDid
              ? { name: "agent", agentDid, section: "agent" }
              : { name: "agents" },
          );
        case "n":
          return go({ name: "session", sessionId: null });
        case "k": {
          e.preventDefault();
          document.querySelector<HTMLTextAreaElement>("textarea")?.focus();
          return;
        }
        case "/":
          e.preventDefault();
          setOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [agentDid]);
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent
        aria-modal="true"
        className="sm:max-w-sm"
        data-testid="shortcuts-help"
      >
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription>
            {IN_DESKTOP
              ? "The same keys across desktop and iOS."
              : "The desktop app’s keys. In a browser, tab and window keys stay with the browser."}
          </DialogDescription>
        </DialogHeader>
        <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-2 text-sm">
          {ROWS.map(([keys, action]) => (
            <div key={keys} className="contents">
              <dt>
                <Kbd>{keys}</Kbd>
              </dt>
              <dd className="text-muted-foreground">{action}</dd>
            </div>
          ))}
        </dl>
      </DialogContent>
    </Dialog>
  );
}

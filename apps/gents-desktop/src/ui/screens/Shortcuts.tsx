import { useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { Kbd } from "@gents/ui/components/kbd";
import { currentRoute, navigate } from "@/lib/router";
import { isMacTauriShell } from "../../lib/shellPlatform";
import { useHomeDid, useSelectedNodeDid } from "@/hooks/useClient";
import { ArrowLeftRight, BellDot, History, PanelLeft } from "lucide-react";
import { sidebar } from "@/app/sidebarChange";
import { useApp } from "@/app/AppContext";
import { switchToNode } from "@/app/NodeSwitcher";
import { previousNode } from "../../hooks/activeNode";
import { sessionToReturnTo } from "../../hooks/sessionTrail";
import { seenMarks } from "@/app/sessionViews";
import { isNew } from "@/lib/seen";
import { defaultScope, recentInScope, scopeContextOf } from "@/lib/scope";
import { nodeOf, sessionKeyOf } from "../../hooks/fleetStore";
import "@/commands/app";
import {
  keysTextOf,
  useCommand,
  useCommands,
  type Command,
  type CommandGroup,
} from "@/commands/commands";
import { useCommandKeys } from "@/commands/dispatch";
import "./SearchPalette";
import { IN_DESKTOP, IS_MAC, MOD } from "./shortcutKeys";

export { MOD } from "./shortcutKeys";

/* keys handled where they apply rather than as commands, named here so the
   reference is whole */
const ELSEWHERE: [string, string][] = [
  [`${MOD}↵`, "Send a follow-up the other way: steer or queue"],
];

const GROUPS: CommandGroup[] = ["Pages", "Nodes", "Panel", "Session", "Help"];

/* Turn steps: ⌥⌘↑/↓ on a Mac. Elsewhere Ctrl+Alt+↑/↓ rotates the screen on
   many Windows PCs (Intel graphics) and switches workspace on some Linux
   desktops, and Ctrl+Shift+↑/↓ selects by paragraph in a text field, so
   they are Alt+↑/↓, outside text fields. */
const TURN_KEYS = {
  previous: {
    mac: { key: "ArrowUp", mod: true, alt: true },
    other: { key: "ArrowUp", alt: true },
  },
  next: {
    mac: { key: "ArrowDown", mod: true, alt: true },
    other: { key: "ArrowDown", alt: true },
  },
};

/* the session a route shows, if it shows one */
const openSessionId = () => {
  const here = currentRoute();
  return here?.name === "session" ? here.sessionId : null;
};

const transcriptScroller = () =>
  document.querySelector<HTMLElement>(
    '[data-slot="scroll-area-viewport"][data-following]',
  );

/* the composer on screen, New session's or an open session's; its field is
   an editable element, not a textarea */
const shownComposerField = () =>
  [
    ...document.querySelectorAll<HTMLElement>(
      '[data-slot="composer"] [contenteditable="true"]',
    ),
  ].find((field) => !field.closest("[hidden]")) ?? null;

const step = (by: -1 | 1) => {
  const scroller = transcriptScroller();
  if (!scroller) return false;
  scroller.dispatchEvent(new CustomEvent("transcript:step", { detail: by }));
};

/** The app's key listener, the commands that belong to no one page, and the
    reference that lists every command's keys. */
export function Shortcuts() {
  const nodeDid = useSelectedNodeDid();
  const homeDid = useHomeDid();
  const { actions, stores } = useApp();
  const sessionById = (id: string | null) =>
    id === null
      ? null
      : (Object.values(stores.fleet.getState().sessions).find(
          (s) => s.sessionId === id,
        ) ?? null);
  const goToSession = (s: { sessionId: string; nodeDid: string }) =>
    navigate({ name: "session", sessionId: s.sessionId, nodeDid: s.nodeDid });
  /* sessions in the sidebar's scope that finished since they were last on
     screen here, newest first, other than the one open */
  const newSessions = () => {
    const ctx = scopeContextOf(stores.fleet.getState(), nodeDid, homeDid ?? null);
    const open = openSessionId();
    const since = seenMarks.since();
    return recentInScope(defaultScope("recents"), ctx, Infinity).filter(
      (s) =>
        !s.archivedAt &&
        s.sessionId !== open &&
        isNew(s, seenMarks.of(sessionKeyOf(s)), since),
    );
  };
  const listed = (did: string) => nodeOf(stores.fleet.getState(), did) !== null;
  const [open, setOpen] = useState(false);
  useCommandKeys();
  useCommand({
    id: "page.configuration",
    title: "Go to the node's configuration",
    group: "Pages",
    keys: { key: "3", mod: true },
    desktopOnly: true,
    run: () =>
      navigate(
        nodeDid ? { name: "agent", nodeDid, section: "agent" } : { name: "agents" },
      ),
  });
  useCommand({
    id: "node.last",
    title: "Switch to last node",
    group: "Nodes",
    keys: { key: "o", mod: true, shift: true },
    desktopOnly: true,
    icon: History,
    when: () => {
      const last = previousNode();
      return last !== null && last !== nodeDid && listed(last);
    },
    run: () => {
      const last = previousNode();
      if (last) switchToNode(last, actions.selectNode);
    },
  });
  useCommand({
    id: "session.previous",
    title: "Go to previous session",
    group: "Session",
    keys: { key: ";", mod: true },
    desktopOnly: true,
    icon: ArrowLeftRight,
    when: () => sessionById(sessionToReturnTo(openSessionId())) !== null,
    run: () => {
      const s = sessionById(sessionToReturnTo(openSessionId()));
      if (s) goToSession(s);
    },
  });
  useCommand({
    id: "session.next-new",
    title: "Go to next new session",
    group: "Session",
    keys: { key: "a", mod: true, shift: true },
    desktopOnly: true,
    icon: BellDot,
    when: () => newSessions().length > 0,
    run: () => {
      const next = newSessions()[0];
      if (next) goToSession(next);
    },
  });
  useCommand({
    id: "composer.focus",
    title: "Focus the composer",
    group: "Session",
    keys: { key: "l", mod: true },
    desktopOnly: true,
    when: () => shownComposerField() !== null,
    run: () => shownComposerField()?.focus(),
  });
  useCommand({
    id: "transcript.previous-turn",
    title: "Previous turn",
    group: "Session",
    keys: TURN_KEYS.previous,
    yieldsToControls: true,
    notInFields: !IS_MAC,
    when: () => transcriptScroller() !== null,
    run: () => step(-1),
  });
  useCommand({
    id: "transcript.next-turn",
    title: "Next turn",
    group: "Session",
    keys: TURN_KEYS.next,
    yieldsToControls: true,
    notInFields: !IS_MAC,
    when: () => transcriptScroller() !== null,
    run: () => step(1),
  });
  useCommand({
    id: "sidebar.toggle",
    title: "Expand or collapse the sidebar",
    group: "Pages",
    keys: {
      mac: { key: "s", mod: true, ctrl: true },
      other: { key: "s", mod: true, shift: true },
    },
    icon: PanelLeft,
    run: () => sidebar.toggle(),
  });
  useCommand({
    id: "help.shortcuts",
    title: "Show keyboard shortcuts",
    group: "Help",
    keys: { key: "/", mod: true },
    run: () => setOpen((o) => !o),
  });
  const commands = useCommands();
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent
        aria-modal="true"
        className="sm:max-w-md"
        data-testid="shortcuts-help"
      >
        <DialogHeader>
          <DialogTitle>Keyboard shortcuts</DialogTitle>
          <DialogDescription>
            {IN_DESKTOP
              ? "The app’s keys, by where they act."
              : "The desktop app’s keys. In a browser, tab and window keys stay with the browser."}
          </DialogDescription>
        </DialogHeader>
        <div className="grid max-h-[60vh] gap-4 overflow-y-auto">
          {GROUPS.map((group) => (
            <KeyRows
              key={group}
              title={group}
              rows={rowsOf(commands.filter((c) => c.group === group))}
            />
          ))}
          <KeyRows title="Composer" rows={ELSEWHERE} />
          {isMacTauriShell() && (
            <KeyRows
              title="Window"
              rows={[
                ["⌘T", "New tab"],
                ["⌘⇧N", "New window"],
                ["⌘W", "Close tab or window"],
                ["⌃Tab / ⌃⇧Tab", "Next / previous tab"],
              ]}
            />
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}

/* each command with keys, as written on this platform */
const rowsOf = (commands: readonly Command[]): [string, string][] =>
  commands.flatMap((c) => {
    const keys = keysTextOf(c);
    return keys ? [[keys, c.title]] : [];
  });

function KeyRows({ title, rows }: { title: string; rows: [string, string][] }) {
  if (!rows.length) return null;
  return (
    <section className="grid gap-2">
      <h3 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
        {title}
      </h3>
      <dl className="grid grid-cols-[auto_1fr] items-center gap-x-4 gap-y-2 text-sm">
        {rows.map(([keys, action]) => (
          <div key={`${keys}${action}`} className="contents">
            <dt>
              <Kbd>{keys}</Kbd>
            </dt>
            <dd className="text-muted-foreground">{action}</dd>
          </div>
        ))}
      </dl>
    </section>
  );
}

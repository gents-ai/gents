import { useEffect, useRef, type ComponentType } from "react";
import { createRegistry } from "@/app/registry";
import { IN_DESKTOP, IS_MAC } from "@/screens/shortcutKeys";
import { chordAria, chordText, effectiveChord, type Chord, type KeySpec } from "./keys";

export type CommandGroup = "Pages" | "Nodes" | "Panel" | "Session" | "Help";

/** Something a person can do from a key, the shortcuts reference and the
    search palette, declared once. */
export type Command = {
  id: string;
  title: string;
  group: CommandGroup;
  /** its key, or a family of keys (each passed to `run` by its index) */
  keys?: KeySpec | readonly KeySpec[];
  /** how a family of keys is written, e.g. "⌃1–9" */
  keysText?: string;
  /** a browser keeps the key for itself, so it works only in the app */
  desktopOnly?: boolean;
  /** the key belongs to a control that has focus and arrow keys of its own
      (a list, a menu, a divider) before it belongs to the command */
  yieldsToControls?: boolean;
  /** the key is typing's in a text field, so it acts only outside one */
  notInFields?: boolean;
  /** whether it can run here and now; a key it can't use goes on to the page */
  when?: () => boolean;
  /** runs it; returning false leaves the key to the page */
  run: (index: number) => boolean | void;
  icon?: ComponentType<{ className?: string }>;
  /** listed in the search palette's Actions; true unless said otherwise */
  palette?: boolean;
};

const registry = createRegistry<Command>();

export const registerCommand = registry.register;
export const getCommand = registry.get;
export const listCommands = registry.list;
export const useCommands = registry.useList;
export const clearCommands = registry.clear;

/** Registers a command for as long as the component is mounted. Its `run`
    and `when` are read when it's invoked, so they may close over state. */
export function useCommand(command: Command) {
  const latest = useRef(command);
  latest.current = command;
  const {
    id,
    title,
    group,
    keysText,
    desktopOnly,
    yieldsToControls,
    notInFields,
    icon,
    palette,
  } = command;
  const keys = JSON.stringify(command.keys ?? null);
  useEffect(
    () =>
      registerCommand({
        ...latest.current,
        when: () => latest.current.when?.() ?? true,
        run: (index) => latest.current.run(index),
      }),
    // the key spec is compared by value; run and when are read through `latest`
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [
      id,
      title,
      group,
      keys,
      keysText,
      desktopOnly,
      yieldsToControls,
      notInFields,
      icon,
      palette,
    ],
  );
}

const specs = (keys: Command["keys"]): readonly KeySpec[] =>
  !keys ? [] : Array.isArray(keys) ? keys : [keys as KeySpec];

/** The chords a command answers to on this platform, by index. */
export const chordsOf = (command: Pick<Command, "id" | "keys">, mac = IS_MAC) =>
  specs(command.keys).map((spec, i) =>
    effectiveChord(i === 0 ? command.id : `${command.id}#${i}`, spec, mac),
  );

/** Whether the command's keys work here: in a browser, a desktop-only key
    stays with the browser. */
export const keysWorkHere = (command: Pick<Command, "desktopOnly">) =>
  IN_DESKTOP || !command.desktopOnly;

/** How a command's keys are written, for the reference and tooltips. */
export function keysTextOf(command: Command, mac = IS_MAC): string | undefined {
  if (command.keysText) return command.keysText;
  const chords = chordsOf(command, mac).filter((c): c is Chord => c !== null);
  return chords.length ? chords.map((c) => chordText(c, mac)).join(" / ") : undefined;
}

const hintOf = (command: Command | null) =>
  command && keysWorkHere(command) ? keysTextOf(command) : undefined;

const ariaOf = (command: Command | null) => {
  if (!command || !keysWorkHere(command)) return undefined;
  const chords = chordsOf(command).filter((c): c is Chord => c !== null);
  return chords.length ? chords.map((c) => chordAria(c)).join(" ") : undefined;
};

/** A command's key, for a tooltip, where the key works. */
export const keysHint = (id: string) => hintOf(getCommand(id));
/** The same, following the command as it's registered and unregistered. */
export const useKeysHint = (id: string) => hintOf(registry.useItem(id));

/** A command's keys for `aria-keyshortcuts`, where they work. */
export const ariaKeys = (id: string) => ariaOf(getCommand(id));
export const useAriaKeys = (id: string) => ariaOf(registry.useItem(id));

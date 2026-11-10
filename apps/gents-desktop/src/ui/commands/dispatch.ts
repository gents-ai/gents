import { useEffect } from "react";
import { IS_MAC, keyTaken } from "@/screens/shortcutKeys";
import { chordsOf, keysWorkHere, listCommands, type Command } from "./commands";
import { matches } from "./keys";

const inField = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  (target.isContentEditable || target.closest("input, textarea, select") !== null);

/** Runs the command a key press asks for, if one can run here, and takes
    the key from the page when it does. A key already taken, a desktop-only
    key in a browser, and a command that can't run now all leave the key to
    the page. */
export function dispatchKey(
  e: KeyboardEvent,
  commands: readonly Command[] = listCommands(),
  mac = IS_MAC,
): boolean {
  if (e.defaultPrevented || e.isComposing) return false;
  for (const command of commands) {
    const index = chordsOf(command, mac).findIndex((c) => c && matches(c, e, mac));
    if (index < 0) continue;
    if (!keysWorkHere(command)) return false;
    if (command.yieldsToControls && keyTaken(e)) return false;
    if (command.notInFields && inField(e.target)) return false;
    if (command.when && !command.when()) continue;
    if (command.run(index) === false) continue;
    e.preventDefault();
    return true;
  }
  return false;
}

/** The app's one listener for its commands' keys. */
export function useCommandKeys() {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => void dispatchKey(e);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}

/** The listener as a component, for a tree without the app's shortcuts. */
export function CommandKeys() {
  useCommandKeys();
  return null;
}

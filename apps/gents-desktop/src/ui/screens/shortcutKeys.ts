/* the app's keys as people see them, kept apart from the shortcut handler so
   a tooltip can name one without loading it */
export const IS_MAC = navigator.platform.toUpperCase().includes("MAC");
export const MOD = IS_MAC ? "⌘" : "Ctrl+";
/** the mailbox stack's key (MailboxStack) */
export const MAILBOX_KEYS = IS_MAC ? "⌘⇧M" : "Ctrl+Shift+M";
export const IN_DESKTOP = "__TAURI_INTERNALS__" in window || "__TAURI__" in window;

/** Whether the app's command modifier, and not the other one, is held: ⌘
    on a Mac, where ⌃ keys belong to text editing (⌃K, ⌃N) and to Spaces
    (⌃1–3), and Ctrl elsewhere, where Meta is the system's own key. */
export const commandKey = (
  e: Pick<KeyboardEvent, "metaKey" | "ctrlKey">,
  mac = IS_MAC,
) => (mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey);

/* controls that move with arrow keys of their own: a key pressed in one is
   theirs, not the app's history or the transcript behind */
const OWN_KEYS =
  '[role="tablist"], [role="separator"], [role="dialog"], [role="alertdialog"], [role="listbox"], [role="menu"], [role="menubar"], [role="radiogroup"], [role="slider"]';

/** Whether a key was already taken, or was pressed in a control with arrow
    keys of its own, so an app-wide key leaves it alone. */
export const keyTaken = (e: KeyboardEvent) =>
  e.defaultPrevented ||
  (e.target instanceof Element && e.target.closest(OWN_KEYS) !== null);
export const DESKTOP_ONLY = new Set(["1", "2", "3", "n", "l", ","]);
/** the keys that go to a page, for its tooltip; a browser keeps them */
export const PAGE_KEYS = IN_DESKTOP
  ? {
      nodes: `${MOD}1`,
      sessions: `${MOD}2`,
      newSession: `${MOD}N`,
      settings: `${MOD},`,
    }
  : {
      nodes: undefined,
      sessions: undefined,
      newSession: undefined,
      settings: undefined,
    };

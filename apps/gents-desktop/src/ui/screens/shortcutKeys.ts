/* the app's keys as people see them, kept apart from the shortcut handler so
   a tooltip can name one without loading it */
export const IS_MAC = navigator.platform.toUpperCase().includes("MAC");
export const MOD = IS_MAC ? "⌘" : "Ctrl+";
/** the mailbox stack's key (MailboxStack) */
export const MAILBOX_KEYS = IS_MAC ? "⌘⇧M" : "Ctrl+Shift+M";
export const IN_DESKTOP = "__TAURI_INTERNALS__" in window || "__TAURI__" in window;
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

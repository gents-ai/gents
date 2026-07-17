import { useEffect, useRef } from "react";

export type AppShortcutHandlers = {
  setView: (view: "fleet" | "chat" | "code" | "config") => void;
  newConversation: () => void;
  focusComposer: () => void;
  toggleHelp: () => void;
};

/// App-level keyboard shortcuts: Cmd/Ctrl+1..4 switch views, Cmd/Ctrl+N
/// starts a conversation, Cmd/Ctrl+K focuses the composer, Cmd/Ctrl+/ shows
/// the shortcut reference. Handlers ride a ref so the window listener is
/// installed exactly once.
export function useAppShortcuts(handlers: AppShortcutHandlers) {
  const handlersRef = useRef(handlers);
  handlersRef.current = handlers;

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) {
        return;
      }
      const current = handlersRef.current;
      switch (event.key) {
        case "1":
          current.setView("fleet");
          break;
        case "2":
          current.setView("chat");
          break;
        case "3":
          current.setView("code");
          break;
        case "4":
          current.setView("config");
          break;
        case "n":
          current.newConversation();
          break;
        case "k":
          current.focusComposer();
          break;
        case "/":
          current.toggleHelp();
          break;
        default:
          return;
      }
      event.preventDefault();
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);
}

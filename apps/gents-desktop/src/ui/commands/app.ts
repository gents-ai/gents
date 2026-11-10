import {
  Inbox,
  LayoutGrid,
  ListTree,
  Package,
  Play,
  Plus,
  Settings,
} from "lucide-react";
import { navigate } from "@/lib/router";
import { flag } from "@/preferences";
import { registerCommand } from "./commands";

/* The pages every window can go to. Registered once, on load, so tooltips
   and the search palette have them before anything mounts; the search
   palette opens on the first of them, in this order. */
registerCommand({
  id: "page.new-session",
  title: "New session",
  group: "Pages",
  keys: { key: "n", mod: true },
  desktopOnly: true,
  icon: Plus,
  run: () => navigate({ name: "session", sessionId: null }),
});
registerCommand({
  id: "page.sessions",
  title: "Go to sessions",
  group: "Pages",
  keys: { key: "2", mod: true },
  desktopOnly: true,
  icon: ListTree,
  run: () => navigate({ name: "sessions" }),
});
registerCommand({
  id: "page.automations",
  title: "Go to automations",
  group: "Pages",
  icon: Play,
  when: () => flag("automations"),
  run: () => navigate({ name: "automations" }),
});
registerCommand({
  id: "page.packs",
  title: "Go to packs",
  group: "Pages",
  icon: Package,
  when: () => flag("packs"),
  run: () => navigate({ name: "packs" }),
});
registerCommand({
  id: "page.mailbox",
  title: "Go to mailbox",
  group: "Pages",
  icon: Inbox,
  run: () => navigate({ name: "mailbox" }),
});
registerCommand({
  id: "page.nodes",
  title: "Go to nodes",
  group: "Pages",
  keys: { key: "1", mod: true },
  desktopOnly: true,
  icon: LayoutGrid,
  run: () => navigate({ name: "agents" }),
});
registerCommand({
  id: "page.settings",
  title: "Open settings",
  group: "Pages",
  keys: { key: ",", mod: true },
  desktopOnly: true,
  icon: Settings,
  run: () => navigate({ name: "settings" }),
});

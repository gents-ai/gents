import {
  listenToWindow,
  NOTIFICATION_CLICKED_EVENT,
  notificationsGranted,
  postClickableNotification,
  postNotification,
  requestNotifications,
} from "../../../lib/nativeShell";
import { IN_DESKTOP, IS_MAC } from "@/screens/shortcutKeys";

export type NotifyPermission = "granted" | "denied" | "default";

export type Notice = {
  title: string;
  body?: string;
  /** called when the notification is clicked, where the platform says so */
  onClick?: () => void;
};

/** The system's notifications: the desktop app's plugin, or the browser's
    Notification API, or nothing where neither exists. */
export type NotifyPlatform = {
  permission(): Promise<NotifyPermission>;
  request(): Promise<NotifyPermission>;
  /** settles once the system took the notice, or fails with why it
      didn't; the system may still drop it after taking it (Focus, or
      notifications switched off for the app), and nothing reports that.
      `reportsClick: false` says this one notice's click won't reach
      `onClick` although the platform's usually do. */
  show(notice: Notice): Promise<void | { reportsClick: boolean }>;
  /** whether a click reaches `onClick`; where it doesn't, the click only
      brings the window forward */
  reportsClicks: boolean;
};

/* the notices whose click this view waits to hear about, by the id they
   were posted under; the oldest go first past the cap, since the system
   keeps a notice in Notification Center for as long as the person lets it */
const CLICKS_KEPT = 64;

export function clickRouter(listen: (heard: (id: string) => void) => Promise<unknown>) {
  const clicks = new Map<string, () => void>();
  let listening: Promise<unknown> | null = null;
  return {
    /** waits on `onClick` under a new id, once the view is listening */
    async expect(onClick: (() => void) | undefined): Promise<string> {
      listening ??= listen((id) => {
        const clicked = clicks.get(id);
        clicks.delete(id);
        clicked?.();
      });
      await listening;
      const id = crypto.randomUUID();
      if (onClick) {
        clicks.set(id, onClick);
        for (const old of clicks.keys()) {
          if (clicks.size <= CLICKS_KEPT) break;
          clicks.delete(old);
        }
      }
      return id;
    },
    forget(id: string) {
      clicks.delete(id);
    },
  };
}

/* on macOS the shell waits on each notice and reports its click; elsewhere
   the plugin's notice is fire and forget */
const macClicks = clickRouter((heard) =>
  listenToWindow<string>(NOTIFICATION_CLICKED_EVENT, heard),
);

const desktop: NotifyPlatform = {
  async permission() {
    return (await notificationsGranted()) ? "granted" : "default";
  },
  request: () => requestNotifications(),
  show: async ({ title, body, onClick }) => {
    if (!IS_MAC) return postNotification({ title, body });
    const id = await macClicks.expect(onClick);
    try {
      const posted = await postClickableNotification({ title, body, id });
      if (!posted.reportsClick) macClicks.forget(id);
      return posted;
    } catch (error) {
      macClicks.forget(id);
      throw error;
    }
  },
  reportsClicks: IS_MAC,
};

const browser: NotifyPlatform = {
  async permission() {
    return Notification.permission;
  },
  async request() {
    return Notification.requestPermission();
  },
  async show({ title, body, onClick }) {
    if (Notification.permission !== "granted")
      throw new Error("Notifications aren't allowed for this page");
    const shown = new Notification(title, { body });
    shown.onclick = () => {
      window.focus();
      onClick?.();
      shown.close();
    };
  },
  reportsClicks: true,
};

const none: NotifyPlatform = {
  async permission() {
    return "denied";
  },
  async request() {
    return "denied";
  },
  async show() {
    throw new Error("This build can't show notifications");
  },
  reportsClicks: false,
};

export const notifyPlatform: NotifyPlatform = IN_DESKTOP
  ? desktop
  : typeof Notification === "function"
    ? browser
    : none;

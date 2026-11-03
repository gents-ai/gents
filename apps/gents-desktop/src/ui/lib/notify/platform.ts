import {
  notificationsGranted,
  postNotification,
  requestNotifications,
} from "../../../lib/nativeShell";
import { IN_DESKTOP } from "@/screens/shortcutKeys";

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
      notifications switched off for the app), and nothing reports that */
  show(notice: Notice): Promise<void>;
  /** whether a click reaches `onClick`; where it doesn't, the click only
      brings the window forward */
  reportsClicks: boolean;
};

const desktop: NotifyPlatform = {
  async permission() {
    return (await notificationsGranted()) ? "granted" : "default";
  },
  request: () => requestNotifications(),
  show: ({ title, body }) => postNotification({ title, body }),
  reportsClicks: false,
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

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
  show(notice: Notice): void;
  /** whether a click reaches `onClick`; where it doesn't, the click only
      brings the window forward */
  reportsClicks: boolean;
};

const plugin = () => import("@tauri-apps/plugin-notification");

/* the plugin answers only granted or not; asking again is what tells a
   refusal from a question never asked */
const desktop: NotifyPlatform = {
  async permission() {
    return (await plugin())
      .isPermissionGranted()
      .then((on) => (on ? "granted" : "default"));
  },
  async request() {
    return (await plugin()).requestPermission();
  },
  show({ title, body }) {
    void plugin().then(({ sendNotification }) => sendNotification({ title, body }));
  },
  reportsClicks: false,
};

const browser: NotifyPlatform = {
  async permission() {
    return Notification.permission;
  },
  async request() {
    return Notification.requestPermission();
  },
  show({ title, body, onClick }) {
    if (Notification.permission !== "granted") return;
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
  show() {},
  reportsClicks: false,
};

export const notifyPlatform: NotifyPlatform = IN_DESKTOP
  ? desktop
  : typeof Notification === "function"
    ? browser
    : none;

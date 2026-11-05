import { toast } from "sonner";
import type { UxNotifyCaller } from "@/contrib/plugin";
import { flag, pluginNotifyOn } from "@/preferences";
import { inNativeShell, windowLabel } from "../../../lib/nativeShell";
import { isPluginSubpath, linkRoute } from "../links";
import type { Route } from "../router";
import type { Notice } from "./platform";
import { createTeller, defaultTellerDeps } from "./watch";

export const NOTIFY_TITLE_MAX = 80;
export const NOTIFY_BODY_MAX = 160;
/** how many notifications one plugin may raise within `NOTIFY_RATE_WINDOW` */
export const NOTIFY_RATE_COUNT = 5;
export const NOTIFY_RATE_WINDOW = 60_000;

export type PluginNotifyDeps = {
  /** both experimental features, Notifications and Notifications from packs */
  enabled: () => boolean;
  /** this window is the one that tells: every window runs its own copy of
      each plugin, and only the main one speaks for them */
  speaks: () => boolean;
  switchedOn: (key: string, kind: string) => boolean;
  /** the window is in front and shown */
  focused: () => boolean;
  now: () => number;
  /** through the system, opening `to` when clicked */
  tell: (notice: Notice & { to: Route }) => void;
  /** in the app, while its window is in front */
  show: (notice: Notice, open: () => void) => void;
  open: (to: Route) => void;
};

const defaults = (): PluginNotifyDeps => {
  const base = defaultTellerDeps();
  const teller = createTeller(base);
  return {
    enabled: () => flag("notifications") && flag("pack-notifications"),
    speaks: () => !inNativeShell() || windowLabel() === "main",
    switchedOn: pluginNotifyOn,
    focused: base.focused,
    now: base.now,
    tell: teller.tell,
    show: (notice, open) =>
      void toast(notice.title, {
        description: notice.body,
        action: { label: "Open", onClick: open },
      }),
    open: base.open,
  };
};

/* one line of plain text: what a notification shows, nothing it could
   format or hide */
const oneLine = (value: string) =>
  value
    .replace(/[\p{Cc}\p{Zl}\p{Zp}]/gu, " ")
    .replace(/\s+/g, " ")
    .trim();

/** The owner of what plugins may tell the person. A plugin asks for one of
    the kinds its module declared; it is told only while both experimental
    features are on, in the main window, for a kind the person turned on for
    that plugin, at most `NOTIFY_RATE_COUNT` a minute. A click opens the
    plugin's own page, or a place within it, through the route a `gents:`
    link to it takes; no other target can be named.

    What a notification may carry once it leaves the app is not decided
    yet: it can show on a locked screen and stays in Notification Center,
    beyond the app's own access checks. Until it is, a notice carries the plugin's name, a one-line title and an optional
    one-line body, each length-limited, and nothing else of the plugin's. */
export function createPluginNotifier(deps: PluginNotifyDeps = defaults()) {
  const sent = new Map<string, number[]>();
  return async (caller: UxNotifyCaller, request: unknown): Promise<void> => {
    const refuse = (why: string): never => {
      throw new Error(`${caller.id}: ctx.notify: ${why}`);
    };
    if (!deps.enabled())
      refuse(
        "notifications from packs are off; the person turns them on under Experimental features",
      );
    if (!deps.speaks()) refuse("only the app's main window shows notifications");
    if (typeof request !== "object" || request === null || Array.isArray(request))
      refuse("pass { kind, title, body?, target? }");
    const { kind, title, body, target } = request as Record<string, unknown>;
    const declared = caller.kinds.find((k) => k.kind === kind);
    if (!declared)
      refuse(
        `kind ${JSON.stringify(kind)} is not declared (declared: ${caller.kinds.map((k) => k.kind).join(", ") || "none"})`,
      );
    if (!deps.switchedOn(caller.key, declared!.kind))
      refuse(
        `"${declared!.label}" is off; the person turns it on under Settings, Notifications`,
      );
    const line = typeof title === "string" ? oneLine(title) : "";
    if (!line || line.length > NOTIFY_TITLE_MAX)
      refuse(`title must be text of 1 to ${NOTIFY_TITLE_MAX} characters`);
    if (body !== undefined && typeof body !== "string")
      refuse("body must be text when given");
    const detail = typeof body === "string" ? oneLine(body) : "";
    if (detail.length > NOTIFY_BODY_MAX)
      refuse(`body must be at most ${NOTIFY_BODY_MAX} characters`);
    if (
      target !== undefined &&
      (typeof target !== "string" || !isPluginSubpath(target))
    )
      refuse(
        "target must be a place within the plugin's own page, as path segments (`items/42`)",
      );
    const now = deps.now();
    const recent = (sent.get(caller.key) ?? []).filter(
      (at) => now - at < NOTIFY_RATE_WINDOW,
    );
    if (recent.length >= NOTIFY_RATE_COUNT)
      refuse(`at most ${NOTIFY_RATE_COUNT} notifications a minute`);
    sent.set(caller.key, [...recent, now]);
    const to = linkRoute({
      kind: "plugin",
      pluginId: caller.id,
      ...(typeof target === "string" ? { path: target } : {}),
    });
    const notice = {
      title: `${oneLine(caller.name)}: ${line}`,
      ...(detail ? { body: detail } : {}),
    };
    if (deps.focused()) deps.show(notice, () => deps.open(to));
    else deps.tell({ ...notice, to });
  };
}

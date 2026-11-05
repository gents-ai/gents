import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { notifyPlatform, type NotifyPermission } from "@/lib/notify/platform";
import type { NotifyKind } from "@/lib/notify/events";
import { IS_MAC } from "@/screens/shortcutKeys";
import { errorText } from "../../../lib/errorText";
import { notifierKey } from "@/contrib/activate";
import { useUxPluginRecords, type UxPluginRecord } from "@/contrib/plugins-store";
import {
  pluginNotifySwitch,
  preferences,
  useFlag,
  useNotifyKinds,
  usePluginNotify,
} from "@/preferences";
import { SwitchRow } from "../agent/editors";
import { Group, Row } from "../agent/rows";

const KINDS: { kind: NotifyKind; label: string; description: string }[] = [
  { kind: "finished", label: "Finished", description: "A session finished its work." },
  {
    kind: "failed",
    label: "Failed",
    description: "A session, or a worker one of them started, failed.",
  },
  {
    kind: "question",
    label: "Needs your answer",
    description: "An agent asked you a question.",
  },
  {
    kind: "mailbox",
    label: "Mailbox items",
    description: "Something new arrived in the mailbox.",
  },
];

const REFUSED = IS_MAC
  ? "Notifications are turned off for Gents. To allow them, open System Settings, choose Notifications, then Gents, and turn on Allow notifications."
  : "Notifications are turned off for Gents. To allow them, turn them on for Gents in your system's notification settings.";

/** How the app's notifications reach this device. Showing it is what asks
    the system for permission, the first time. */
/* a development build is sent as Terminal by the plugin, so its notices
   follow Terminal's notification settings, not the app's */
const TEST_SENT = import.meta.env.DEV
  ? "Sent. This development build's notifications come from Terminal: if nothing appeared, allow Terminal in System Settings, Notifications, and check Focus."
  : IS_MAC
    ? "Sent. If nothing appeared, check that Gents is allowed in System Settings, Notifications, and that Focus is off."
    : "Sent. If nothing appeared, check that notifications are on for Gents in your system's settings.";

export function NotificationsGroup() {
  const fromPacks = useFlag("pack-notifications");
  return (
    <>
      <AppNotifications />
      {fromPacks && <PluginNotifications />}
    </>
  );
}

const switchId = (key: string, kind: string) =>
  `settings-notify-plugin-${`${key}-${kind}`.replace(/[^A-Za-z0-9_-]/g, "-")}`;

const origin = (record: UxPluginRecord) =>
  record.pack
    ? `From the ${record.pack} pack.`
    : record.door === "bundled"
      ? "Built into the app."
      : "From a plugin in your home folder.";

/** Each kind of notification a plugin declares, off until the person turns
    it on: a kind is listed once its plugin has loaded, since the module is
    what declares it. */
function PluginNotifications() {
  const records = useUxPluginRecords();
  const on = usePluginNotify();
  const notifying = Object.values(records)
    .filter((r) => (r.notifications?.length ?? 0) > 0)
    .sort((a, b) => a.name.localeCompare(b.name));
  return (
    <Group title="From packs">
      {notifying.length === 0 ? (
        <Row
          label="No pack asks to notify you"
          description="A pack's page that can tell you something lists its kinds here once it has loaded, each off until you turn it on."
        />
      ) : (
        notifying.flatMap((record) => {
          const key = notifierKey(record);
          return record.notifications!.map(({ kind, label, description }) => (
            <SwitchRow
              key={`${key} ${kind}`}
              id={switchId(key, kind)}
              label={`${record.name}: ${label}`}
              description={[description, origin(record)].filter(Boolean).join(" ")}
              checked={on.includes(pluginNotifySwitch(key, kind))}
              onChange={(next) => preferences.setPluginNotify(key, kind, next)}
            />
          ));
        })
      )}
    </Group>
  );
}

function AppNotifications() {
  const kinds = useNotifyKinds();
  const [permission, setPermission] = useState<NotifyPermission | null>(null);
  /* what the last test said: the system takes a notice without saying
     whether it showed it, so a sent test only rules out a refusal */
  const [test, setTest] = useState<{ sent: true } | { failed: string } | null>(null);
  const sendTest = () => {
    setTest(null);
    notifyPlatform
      .show({
        title: "Notifications are on",
        body: "The app tells you here when a session finishes or needs you.",
      })
      .then(
        () => setTest({ sent: true }),
        (error: unknown) => setTest({ failed: errorText(error) }),
      );
  };
  useEffect(() => {
    let live = true;
    void notifyPlatform
      .permission()
      .then((now) => (now === "default" ? notifyPlatform.request() : now))
      .then((now) => live && setPermission(now))
      .catch(() => live && setPermission("denied"));
    return () => {
      live = false;
    };
  }, []);
  return (
    <Group title="Notifications">
      <Row
        label="When the window is in the background"
        description={
          permission === "denied"
            ? REFUSED
            : "The app tells you about your sessions through the system's notifications, and stays quiet about the session on screen."
        }
      />
      {KINDS.map(({ kind, label, description }) => (
        <SwitchRow
          key={kind}
          id={`settings-notify-${kind}`}
          label={label}
          description={description}
          checked={kinds[kind]}
          onChange={(on) => preferences.setNotify(kind, on)}
        />
      ))}
      <Row
        label="Test"
        description={
          test && "failed" in test
            ? `Couldn't show a notification: ${test.failed}`
            : test
              ? TEST_SENT
              : "Shows a notification now, even with the window in front."
        }
      >
        <Button
          variant="outline"
          size="sm"
          disabled={permission !== "granted"}
          onClick={sendTest}
        >
          Send a test notification
        </Button>
      </Row>
    </Group>
  );
}

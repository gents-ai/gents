import { useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { notifyPlatform, type NotifyPermission } from "@/lib/notify/platform";
import type { NotifyKind } from "@/lib/notify/events";
import { IS_MAC } from "@/screens/shortcutKeys";
import { preferences, useNotifyKinds } from "@/preferences";
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
export function NotificationsGroup() {
  const kinds = useNotifyKinds();
  const [permission, setPermission] = useState<NotifyPermission | null>(null);
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
        description="Shows a notification now, even with the window in front."
      >
        <Button
          variant="outline"
          size="sm"
          disabled={permission !== "granted"}
          onClick={() =>
            notifyPlatform.show({
              title: "Notifications are on",
              body: "The app tells you here when a session finishes or needs you.",
            })
          }
        >
          Send a test notification
        </Button>
      </Row>
    </Group>
  );
}

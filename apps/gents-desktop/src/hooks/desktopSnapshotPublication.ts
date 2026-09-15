import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

export type SnapshotPublication = {
  isCurrent: () => boolean;
  publish: (snapshot: DesktopClientSnapshot) => boolean;
};

/**
 * One ordering owner for every asynchronous desktop snapshot producer.
 * Capturing is the operation's issuance point; only the newest capture may
 * publish, regardless of completion order.
 */
export function createSnapshotPublicationOwner(
  publish: (snapshot: DesktopClientSnapshot) => void,
) {
  let generation = 0;

  return {
    begin(): SnapshotPublication {
      generation += 1;
      const captured = generation;
      const isCurrent = () => captured === generation;
      return {
        isCurrent,
        publish: (snapshot) => {
          if (!isCurrent()) return false;
          publish(snapshot);
          return true;
        },
      };
    },
  };
}

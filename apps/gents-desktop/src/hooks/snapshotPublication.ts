import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

export type SnapshotPublication = {
  isCurrent: () => boolean;
  publish: (snapshot: DesktopClientSnapshot) => boolean;
};

/** Orders reads, not mutations: only the newest issued read may publish. */
export function createSnapshotPublicationOwner(
  publish: (snapshot: DesktopClientSnapshot) => void,
) {
  let generation = 0;
  let latest: DesktopClientSnapshot | null = null;
  function checkpoint() {
    const captured = generation;
    return () => captured === generation;
  }

  return {
    get snapshot() {
      return latest;
    },
    checkpoint,
    begin(): SnapshotPublication {
      generation += 1;
      const isCurrent = checkpoint();
      return {
        isCurrent,
        publish: (snapshot) => {
          if (!isCurrent()) return false;
          latest = snapshot;
          publish(snapshot);
          return true;
        },
      };
    },
  };
}

import { toast } from "sonner";

const DAY = 24 * 60 * 60;

const days = (seconds: number) => {
  const n = Math.round(seconds / DAY);
  return `${n} ${n === 1 ? "day" : "days"}`;
};

/** Says a request to join was approved, once the node confirms it, with Undo
    revoking it again. */
export function sayApproved(
  nodeName: string,
  leaseSeconds: number,
  undo: () => Promise<void>,
) {
  toast.success(`Approved: a device can use ${nodeName} for ${days(leaseSeconds)}`, {
    action: {
      label: "Undo",
      onClick: () =>
        void undo().then(
          () => toast(`Revoked: that device can't use ${nodeName} any more`),
          (error: unknown) =>
            toast.error(
              `Couldn't undo: ${error instanceof Error ? error.message : String(error)}`,
            ),
        ),
    },
  });
}

/** Says a request to join was denied, once the node confirms it. */
export function sayDenied() {
  toast(`Denied: that device won't get in`);
}

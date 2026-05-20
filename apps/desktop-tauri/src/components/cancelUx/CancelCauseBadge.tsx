import type { JSX } from "react";
import type { DerivedCancelCauseView } from "../../lib/types/operations";

const LABELS: Record<DerivedCancelCauseView["cause"], string> = {
  userCancelled: "user cancelled",
  interrupted: "interrupted",
  deadline: "deadline expired",
  unknown: "cause unknown",
};

export type CancelCauseBadgeProps = {
  cause: DerivedCancelCauseView;
  className?: string;
};

export function CancelCauseBadge({ cause, className }: CancelCauseBadgeProps): JSX.Element {
  const classes = ["cause-badge", `cause-${cause.cause}`];
  if (className) classes.push(className);
  return <span className={classes.join(" ")}>{LABELS[cause.cause] ?? cause.cause}</span>;
}

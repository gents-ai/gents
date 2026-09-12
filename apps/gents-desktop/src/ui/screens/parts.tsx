import type { ComponentProps } from "react";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { cn } from "@gents/ui/lib/utils";
import { behaviorName, initials } from "./behavior";
import { BehaviorHoverCard } from "./HoverCards";

/* two-letter initials inside a quiet ring: raised face, hairline border,
   ordinary text ink. Per-behaviour colour is parked for now; the hue
   helpers in behavior.ts stay for when it comes back. */
/* forwards every span prop, so a hover-card or tooltip trigger can render it */
export function BehaviorAvatar({
  name,
  behaviorId: _behaviorId,
  className,
  ...props
}: { name: string; behaviorId?: string | null; className?: string } & Omit<
  ComponentProps<"span">,
  "children"
>) {
  return (
    <span
      {...props}
      className={cn(
        "grid size-7 shrink-0 place-items-center rounded-full border border-border bg-raised text-[11px] font-medium text-foreground",
        className,
      )}
      aria-hidden="true"
    >
      {initials(name)}
    </span>
  );
}

export function BehaviorChip({
  behaviorId,
  deployment,
  meta,
  showName = true,
  description,
}: {
  behaviorId: string | null;
  deployment: DeploymentView | null;
  meta?: string;
  showName?: boolean;
  description?: string;
}) {
  const name = behaviorName(behaviorId, deployment);
  return (
    <span className="flex items-center gap-2 text-sm text-muted-foreground">
      <BehaviorHoverCard
        deployment={deployment}
        behaviorId={behaviorId}
        description={description}
      >
        <BehaviorAvatar
          name={name}
          behaviorId={behaviorId}
          className="cursor-default"
        />
      </BehaviorHoverCard>
      {showName && (
        <span className="whitespace-nowrap">
          {name}
          {meta && <span> ◦ {meta}</span>}
        </span>
      )}
    </span>
  );
}

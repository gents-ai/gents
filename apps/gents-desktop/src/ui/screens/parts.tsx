import type { ComponentProps } from "react";
import { cn } from "@gents/ui/lib/utils";
import { initials } from "./behavior";

/* two-letter initials inside a quiet ring: raised face, hairline border,
   ordinary text ink */
/* forwards every span prop, so a hover-card or tooltip trigger can render it */
export function BehaviorAvatar({
  name,
  className,
  ...props
}: { name: string; className?: string } & Omit<ComponentProps<"span">, "children">) {
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

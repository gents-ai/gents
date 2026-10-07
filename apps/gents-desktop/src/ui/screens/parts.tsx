import type { ComponentProps } from "react";
import { cn } from "@gents/ui/lib/utils";
import type { BehaviorEnvironmentView } from "@source-inc/gents-desktop-client";
import { bashAccess, fileAccess, initials, network } from "./behavior";

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

/** What a behavior may do, as one sentence: files and commands, then the
    network. Its environment is read from the node, so it may still be
    loading. */
export function AccessSentence({
  name,
  env,
}: {
  name: string;
  env:
    | Pick<BehaviorEnvironmentView, "fileAccess" | "bashAccess" | "networkAccess">
    | undefined;
}) {
  return (
    <>
      {name} <strong className="font-medium text-foreground">can</strong>{" "}
      {env
        ? `${fileAccess(env.fileAccess)} files and ${bashAccess(env.bashAccess)} commands`
        : "…"}
      , and <strong className="font-medium text-foreground">has access</strong> to{" "}
      {network(env?.networkAccess)}.
    </>
  );
}

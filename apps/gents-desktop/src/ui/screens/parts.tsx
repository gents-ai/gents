import type { ComponentProps } from "react";
import { cn } from "@gents/ui/lib/utils";
import type { AgentEnvironmentView } from "@source-inc/gents-desktop-client";
import { bashAccess, fileAccess, initials, network } from "./behavior";

export function AgentInitials({
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

export function AccessSentence({
  name,
  env,
}: {
  name: string;
  env:
    | Pick<AgentEnvironmentView, "fileAccess" | "bashAccess" | "networkAccess">
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

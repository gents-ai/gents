import { useState, type ComponentProps } from "react";
import { cn } from "@gents/ui/lib/utils";
import { initials } from "./behavior";
import { avatarFor } from "@/lib/avatar";

export function NodeAvatar({
  name,
  src,
  className,
  ...props
}: {
  name: string;
  src?: string | null;
  className?: string;
} & Omit<
  ComponentProps<"span"> & ComponentProps<"img">,
  "name" | "src" | "className"
>) {
  const picture = src === undefined ? avatarFor(name) : src;
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  if (!picture || picture === failedSrc) {
    return (
      <span
        {...(props as ComponentProps<"span">)}
        className={cn(
          "grid size-8 shrink-0 place-items-center rounded-full bg-muted font-heading text-sm text-heading",
          className,
        )}
        aria-hidden="true"
      >
        {initials(name)}
      </span>
    );
  }
  return (
    <img
      {...(props as ComponentProps<"img">)}
      src={picture}
      alt=""
      className={cn("size-8 max-w-none shrink-0 rounded-full object-cover", className)}
      onError={() => setFailedSrc(picture)}
    />
  );
}

import { useLayoutEffect, useRef, useState, type ComponentProps } from "react";
import { cn } from "@gents/ui/lib/utils";
import { initials } from "./behavior";
import { avatarSrc, shownAvatar } from "@/lib/avatar";
import { spinReel, useAvatarReel, useChosenAvatar, type Reel } from "@/lib/avatarRoll";

/** A node's portrait: its chosen avatar, or the one its name picks. With
    `rollId` (the node's DID) it follows that node's rolls, so every avatar
    of the node spins in step. */
export function NodeAvatar({
  name,
  avatar,
  rollId,
  src,
  className,
  ...props
}: {
  name: string;
  avatar?: string | null;
  rollId?: string;
  /** a picture in place of a portrait; null shows initials */
  src?: string | null;
  className?: string;
} & Omit<ComponentProps<"span">, "children" | "className">) {
  const chosen = useChosenAvatar(rollId);
  const { reel, land } = useAvatarReel(rollId);
  const shown = shownAvatar(chosen ?? avatar, name);
  const picture = src === undefined ? avatarSrc(shown) : src;
  const reeling =
    reel && (!reel.landed || reel.run[reel.run.length - 1] === shown) ? reel : null;
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const failed = !picture || picture === failedSrc;
  return (
    <span
      {...props}
      /* the portraits' paper, so one still loading shows blank paper */
      className={cn(
        "relative grid size-8 shrink-0 place-items-center overflow-hidden rounded-full bg-[#fbfaf8]",
        failed && !reeling && "bg-muted font-heading text-sm text-heading",
        className,
      )}
      aria-hidden="true"
    >
      {reeling ? (
        <ReelStrip reel={reeling} onLanded={land} />
      ) : failed ? (
        initials(name)
      ) : (
        <img
          src={picture}
          alt=""
          draggable={false}
          className="size-full max-w-none object-cover"
          onError={() => setFailedSrc(picture)}
        />
      )}
    </span>
  );
}

/** The props that draw a node's own avatar and follow its rolls. */
export const nodeAvatarOf = (node: {
  nodeDid: string;
  nodeConfig?: { avatar?: string | null } | null;
}) => ({ avatar: node.nodeConfig?.avatar ?? null, rollId: node.nodeDid });

function ReelStrip({ reel, onLanded }: { reel: Reel; onLanded: () => void }) {
  const strip = useRef<HTMLSpanElement>(null);
  const landed = useRef(onLanded);
  landed.current = onLanded;
  /* in the commit that lays the reel out, so a roll starts on its click's frame */
  const { run, turn } = reel;
  useLayoutEffect(
    () => spinReel(strip.current!, run.length, () => landed.current()),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one spin per roll
    [turn],
  );
  return (
    <span
      ref={strip}
      className="absolute inset-x-0 top-0 flex flex-col"
      style={{ height: `${run.length * 100}%` }}
    >
      {run.map((key, i) => (
        <img
          key={i}
          src={avatarSrc(key)}
          alt=""
          draggable={false}
          className="min-h-0 w-full max-w-none flex-1 object-cover"
        />
      ))}
    </span>
  );
}

import { startTransition } from "react";
import { Dices } from "lucide-react";
import { cn } from "@gents/ui/lib/utils";
import { randomAvatar, shownAvatar } from "@/lib/avatar";
import { preloadAvatars, rollAvatar, useChosenAvatar } from "@/lib/avatarRoll";
import { NodeAvatar } from "./AgentAvatar";

/** A node's avatar that rolls to a new random one when clicked: a reel of
    portraits runs up through the window, fast at first and easing to a stop
    on the new one, in step with every other avatar of `rollId`, which show
    it until `settleAvatar`. A click mid-roll rolls again. `onChange` takes
    each new one; keeping it is the owner's. On hover it grows to twice its
    size so the portrait can be seen, and a tab with dice slides out from
    under it, leaving the face uncovered. */
export function AvatarRoll({
  rollId,
  name,
  avatar,
  onChange,
  disabled = false,
  className,
}: {
  rollId: string;
  name: string;
  avatar: string | null | undefined;
  onChange: (avatar: string) => void;
  disabled?: boolean;
  className?: string;
}) {
  const chosen = useChosenAvatar(rollId);
  const current = shownAvatar(chosen ?? avatar, name);
  const roll = () => {
    if (disabled) return;
    const to = randomAvatar(current);
    rollAvatar(rollId, current, to);
    /* after the reel's own commit, so the reel moves on the click's frame */
    startTransition(() => onChange(to));
  };

  return (
    <button
      type="button"
      aria-label="Roll a new avatar"
      title="Roll a new avatar"
      disabled={disabled}
      onClick={roll}
      onPointerEnter={preloadAvatars}
      onFocus={preloadAvatars}
      className={cn(
        "group/roll relative size-14 shrink-0 cursor-pointer rounded-full ring-offset-2 ring-offset-background outline-none transition-[scale,box-shadow] duration-200 ease-out hover:z-10 hover:scale-200 hover:shadow-xl hover:ring-1 hover:ring-offset-1 hover:ring-muted-foreground focus-visible:ring-2 focus-visible:ring-ring disabled:cursor-default disabled:scale-100 disabled:shadow-none disabled:ring-0 motion-reduce:transition-none",
        className,
      )}
    >
      {!disabled && (
        <span
          aria-hidden
          className="absolute top-1/2 left-1/2 flex h-[calc(50%+1.25rem)] w-5 -translate-x-1/2 -translate-y-5 items-end justify-center rounded-b-full border border-border bg-muted pb-1 text-muted-foreground opacity-0 transition-[translate,opacity] duration-200 ease-out group-hover/roll:translate-y-0 group-hover/roll:text-foreground group-hover/roll:opacity-100 group-focus-visible/roll:translate-y-0 group-focus-visible/roll:opacity-100 motion-reduce:transition-none"
        >
          <Dices className="size-2.5" />
        </span>
      )}
      <NodeAvatar name={name} avatar={avatar} rollId={rollId} className="size-full" />
    </button>
  );
}

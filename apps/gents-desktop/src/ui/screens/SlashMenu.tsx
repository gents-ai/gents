import { cn } from "@gents/ui/lib/utils";
import type { SlashItem } from "./useSlashMenu";

export function SlashMenu({
  open,
  query,
  items,
  active,
  onPick,
}: {
  open: boolean;
  query: string;
  items: SlashItem[];
  active: number;
  onPick: (item: SlashItem) => void;
}) {
  if (!open) return null;
  const firstSkill = items.findIndex((item) => item.kind === "skill");
  return (
    <div className="absolute inset-x-0 bottom-full z-30 mb-2 animate-in rounded-2xl bg-popover p-1 text-popover-foreground shadow-lg ring-1 ring-foreground/5 duration-100 fade-in-0 slide-in-from-bottom-2 dark:ring-foreground/10">
      {items.length ? (
        <ul role="listbox" aria-label="Commands and skills" className="grid gap-0.5">
          {items.map((item, i) => {
            const id = item.kind === "command" ? item.command.id : item.skill.skillId;
            return (
              <li
                key={`${item.kind}:${id}`}
                className={cn(
                  i === firstSkill && i > 0 && "mt-1 border-t border-border/60 pt-1",
                )}
              >
                <button
                  type="button"
                  role="option"
                  aria-selected={i === active}
                  aria-disabled={
                    (item.kind === "command" && item.command.disabled) || undefined
                  }
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => onPick(item)}
                  className={cn(
                    "flex w-full items-baseline gap-3 rounded-xl px-2 py-1.5 text-left text-sm hover:bg-accent aria-disabled:cursor-default aria-disabled:opacity-50 aria-disabled:hover:bg-transparent",
                    i === active && "bg-accent",
                  )}
                >
                  <span className="font-mono text-xs text-muted-foreground">/{id}</span>
                  {item.kind === "command" ? (
                    <span className="min-w-0 truncate text-muted-foreground">
                      {item.command.description}
                    </span>
                  ) : (
                    <>
                      <span className="min-w-0 truncate">
                        {item.skill.displayName ?? item.skill.name ?? ""}
                      </span>
                      {item.skill.description && (
                        <span className="ml-auto min-w-0 truncate text-xs text-muted-foreground">
                          {item.skill.description}
                        </span>
                      )}
                    </>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      ) : (
        <p className="px-2 py-1.5 text-sm text-muted-foreground">
          No commands or skills match /{query}
        </p>
      )}
    </div>
  );
}

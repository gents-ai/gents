/* The end of a list row: an optional on/off switch and a ⋯ menu with Open,
   Duplicate and Delete. Delete goes through the same typed-name confirm as
   the editor's Danger zone. One pattern for every config list. */
import { useState } from "react";
import { MoreHorizontal } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { Switch } from "@gents/ui/components/switch";
import { href, navigate, type Route } from "@/lib/router";
import { ConfirmDelete, NOUNS } from "./ListDetail";

export function RowMenu({
  name,
  base,
  id,
  enabled,
  onDuplicate,
  onDelete,
  warning,
  strict,
  children,
}: {
  name: string;
  base: Extract<Route, { name: "agent" }>;
  id: string;
  /* the switch; `blocked` says why it cannot be turned on */
  enabled?: {
    checked: boolean;
    onChange: (next: boolean) => Promise<unknown>;
    blocked?: string;
  };
  /* makes the copy and resolves with its id; the row opens it */
  onDuplicate?: () => Promise<string>;
  onDelete?: () => Promise<unknown>;
  warning?: string;
  /* always ask for the name */
  strict?: boolean;
  /* extra menu items, before Delete */
  children?: React.ReactNode;
}) {
  const noun = NOUNS[base.section] ?? "item";
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const toggle = async (next: boolean) => {
    if (!enabled) return;
    setBusy(true);
    try {
      await enabled.onChange(next);
      toast(`${name} is ${next ? "on" : "off"}`);
    } catch (e) {
      toast(
        `Couldn’t turn it ${next ? "on" : "off"}: ${e instanceof Error ? e.message : String(e)}`,
      );
    } finally {
      setBusy(false);
    }
  };
  const duplicate = async () => {
    if (!onDuplicate) return;
    setBusy(true);
    try {
      const copy = await onDuplicate();
      toast(`Duplicated ${name}`);
      navigate({ ...base, item: copy });
    } catch (e) {
      toast(`Duplicate failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="flex items-center gap-1" data-testid="row-menu">
      {enabled && (
        <span
          className="flex items-center px-1"
          title={!enabled.checked && enabled.blocked ? enabled.blocked : undefined}
        >
          <Switch
            aria-label={`${name} is ${enabled.checked ? "on" : "off"}`}
            checked={enabled.checked}
            disabled={busy || (!enabled.checked && Boolean(enabled.blocked))}
            onCheckedChange={(v) => void toggle(v)}
          />
        </span>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button variant="quiet" size="icon-sm" aria-label={`More for ${name}`} />
          }
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-auto min-w-40">
          <DropdownMenuGroup>
            <DropdownMenuItem render={<a href={href({ ...base, item: id })} />}>
              Open
            </DropdownMenuItem>
            {onDuplicate && (
              <DropdownMenuItem disabled={busy} onClick={() => void duplicate()}>
                Duplicate
              </DropdownMenuItem>
            )}
            {children}
          </DropdownMenuGroup>
          {onDelete && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuGroup>
                <DropdownMenuItem
                  variant="destructive"
                  onClick={() => setConfirm(true)}
                >
                  Delete {noun}…
                </DropdownMenuItem>
              </DropdownMenuGroup>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      {onDelete && (
        <ConfirmDelete
          label={name}
          noun={noun}
          open={confirm}
          onOpenChange={setConfirm}
          onDelete={onDelete}
          warning={warning}
          strict={strict}
        />
      )}
    </div>
  );
}

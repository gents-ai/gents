/* The context an agent points at, chosen from the node's contexts, a
   duplicate of one, or an empty one. */
import type { NodeView } from "../../../hooks/fleetStore";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { useEffect } from "react";
import type { AgentContext } from "@source-inc/gents-desktop-client";
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxTrigger,
} from "@gents/ui/components/combobox";
import { nameOf } from "./behaviorDraft";

/* the kit select trigger's look, for a combobox that opens like a select */
const TRIGGER =
  "flex h-9 w-auto min-w-48 max-w-full items-center justify-between gap-1.5 rounded-2xl border border-transparent bg-input/50 px-3 text-sm whitespace-nowrap outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20 max-md:w-full";

/* Picks whose instructions and tools an agent uses. Searchable, because an
   agent can have hundreds of contexts; each row says who uses it. */
export function ContextPicker({
  id,
  deployment,
  value,
  invalid,
  labelFor,
  hint,
  onPick,
  autoOpen = false,
}: {
  id: string;
  deployment: NodeView;
  value: string;
  invalid: boolean;
  labelFor: (value: string) => string;
  hint: (c: AgentContext) => string;
  onPick: (value: string) => void;
  autoOpen?: boolean;
}) {
  /* a searchable popup is a dialog: one at a time with the shell's others */
  const popover = useExclusivePopover();
  const { onOpenChange } = popover;
  useEffect(() => {
    if (autoOpen) onOpenChange(true);
  }, [autoOpen, onOpenChange]);
  const byId = new Map(deployment.contexts.map((c) => [c.context_id, c]));
  return (
    <Combobox
      items={deployment.contexts.map((c) => c.context_id)}
      value={value || null}
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
      onValueChange={(v) => {
        if (v == null) return;
        popover.onOpenChange(false);
        onPick(v);
      }}
      itemToStringLabel={labelFor}
      filter={(v: string, query: string) => {
        const q = query.trim().toLowerCase();
        const c = byId.get(v);
        return (
          !q || (c ? `${nameOf(c)} ${hint(c)}` : labelFor(v)).toLowerCase().includes(q)
        );
      }}
    >
      <ComboboxTrigger
        id={id}
        aria-label="Instructions and tools from"
        aria-invalid={invalid || undefined}
        className={TRIGGER}
      >
        <span className={`truncate ${value ? "" : "text-muted-foreground"}`}>
          {value ? labelFor(value) : "Choose"}
        </span>
      </ComboboxTrigger>
      <ComboboxContent
        ref={popover.popupRef}
        aria-label="Instructions and tools from"
        className="w-96 min-w-96 max-md:w-[calc(100vw-2rem)] max-md:min-w-0"
      >
        <ComboboxInput
          showTrigger={false}
          aria-label="Search contexts"
          placeholder={`Search ${deployment.contexts.length} contexts or agents`}
        />
        <ComboboxEmpty>Nothing by that name.</ComboboxEmpty>
        <ComboboxList>
          {(contextId: string) => {
            const c = byId.get(contextId);
            return (
              <ComboboxItem key={contextId} value={contextId}>
                <span className="flex min-w-0 flex-1 items-baseline justify-between gap-4">
                  <span className="truncate">{c ? nameOf(c) : contextId}</span>
                  <span className="max-w-40 shrink-0 truncate text-xs text-muted-foreground">
                    {c ? hint(c) : ""}
                  </span>
                </span>
              </ComboboxItem>
            );
          }}
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  );
}

import type { ReactElement } from "react";
import { Kbd } from "@gents/ui/components/kbd";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@gents/ui/components/tooltip";

/* the kit's key cap inverts inside a tooltip, but its tooltip uses the
   popover colours, so the cap keeps its own muted look here */
const TOOLTIP_KBD =
  "ml-1.5 in-data-[slot=tooltip-content]:bg-muted in-data-[slot=tooltip-content]:text-muted-foreground dark:in-data-[slot=tooltip-content]:bg-muted";

export function Hint({
  label,
  keys,
  children,
}: {
  label: string;
  keys?: string;
  children: ReactElement;
}) {
  return (
    <TooltipProvider delay={600}>
      <Tooltip>
        <TooltipTrigger render={children} />
        <TooltipContent>
          {label}
          {keys && <Kbd className={TOOLTIP_KBD}>{keys}</Kbd>}
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}

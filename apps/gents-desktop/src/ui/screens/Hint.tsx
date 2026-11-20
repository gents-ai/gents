import type { ReactElement } from "react";
import { Kbd } from "@gents/ui/components/kbd";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@gents/ui/components/tooltip";

export const TOOLTIP_KBD = "ml-1.5";

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

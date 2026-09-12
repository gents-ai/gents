import type { ReactElement } from "react";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@gents/ui/components/tooltip";

/* a delayed tooltip on an icon button: the label appears after a beat,
   so scanning the toolbar does not flash hints */
export function Hint({ label, children }: { label: string; children: ReactElement }) {
  return (
    <TooltipProvider delay={600}>
      <Tooltip>
        <TooltipTrigger render={children} />
        <TooltipContent>{label}</TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}

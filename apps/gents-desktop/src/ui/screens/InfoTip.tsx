/* More about a field, behind an icon beside its label: the page shows the
   field, and what it means is a hover or a keyboard focus away. */
import type { ReactNode } from "react";
import { Info } from "lucide-react";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@gents/ui/components/tooltip";

export function InfoTip({ about, children }: { about: string; children: ReactNode }) {
  return (
    <TooltipProvider delay={200}>
      <Tooltip>
        <TooltipTrigger
          render={
            <button
              type="button"
              aria-label={`About ${about}`}
              className="inline-grid size-4 shrink-0 cursor-help place-items-center rounded-full text-muted-foreground hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
            />
          }
        >
          <Info className="size-3.5" />
        </TooltipTrigger>
        <TooltipContent className="max-w-xs text-xs leading-relaxed">
          {/* one block: the tooltip lays its children out in a row */}
          <span className="block">{children}</span>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}

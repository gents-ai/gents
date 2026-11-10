import { useRef, type ReactNode } from "react";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { cn } from "@gents/ui/lib/utils";
import { useScrollEdges } from "@/lib/scroll";

const FADE = "20px";

/** The nav's middle, between its node and its foot: it takes the room the
    two leave and scrolls when what it holds is taller, so the foot always
    shows. An edge with more past it fades out; a mask, not a painted
    gradient, since the nav sits on more than one ground. */
export function NavScroll({
  className,
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  const { above, below } = useScrollEdges(box);
  const mask = `linear-gradient(to bottom, ${
    above ? `transparent, black ${FADE}` : "black, black"
  }, ${below ? `black calc(100% - ${FADE}), transparent` : "black, black"})`;
  return (
    <ScrollArea
      ref={box}
      data-testid="nav-scroll"
      className={cn("min-h-0 w-full flex-1", className)}
      style={{ maskImage: mask, WebkitMaskImage: mask }}
    >
      {children}
    </ScrollArea>
  );
}

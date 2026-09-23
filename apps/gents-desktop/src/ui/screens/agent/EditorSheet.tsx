/* A document's full editor in a wide sheet beside the page that created it,
   so the page keeps its unsaved draft. Closing returns to the page. */
import { useEffect, useRef, type ReactNode } from "react";
import { ExternalLink } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { href, type Route } from "@/lib/router";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@gents/ui/components/sheet";

export function EditorSheet({
  open,
  onClose,
  title,
  description,
  page,
  children,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  description?: string;
  /* the document's own page, for whoever wants the full view */
  page?: Route;
  children: ReactNode;
}) {
  /* the wheel works over the backdrop too: the sheet is the thing to scroll
     while it is open, and its own strip is narrow */
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onWheel = (e: WheelEvent) => {
      const el = body.current;
      if (!el || el.contains(e.target as Node)) return;
      const sheet = el.closest("[role=dialog]");
      if (sheet?.contains(e.target as Node)) return;
      el.scrollBy({ top: e.deltaY });
      e.preventDefault();
    };
    document.addEventListener("wheel", onWheel, { passive: false });
    return () => document.removeEventListener("wheel", onWheel);
  }, [open]);
  return (
    <Sheet open={open} onOpenChange={(next) => !next && onClose()}>
      <SheetContent
        side="right"
        className="flex w-full flex-col gap-0 border-border/60 max-md:max-w-full data-[side=right]:max-md:w-full md:w-[92vw] data-[side=right]:sm:max-w-3xl"
      >
        <SheetHeader className="pr-20">
          <SheetTitle>{title}</SheetTitle>
          {page && (
            <Button
              variant="quiet"
              size="icon-sm"
              aria-label="Open its page"
              className="absolute top-4 right-12"
              nativeButton={false}
              render={<a href={href(page)} />}
            >
              <ExternalLink />
            </Button>
          )}
          {description && <SheetDescription>{description}</SheetDescription>}
        </SheetHeader>
        <div ref={body} className="min-h-0 flex-1 overflow-y-auto px-4 pb-6">
          {children}
        </div>
      </SheetContent>
    </Sheet>
  );
}

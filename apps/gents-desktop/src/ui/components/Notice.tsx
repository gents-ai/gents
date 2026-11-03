import type { ComponentProps, ReactNode } from "react";
import { CircleAlert, Info, TriangleAlert } from "lucide-react";
import { Alert, AlertTitle } from "@gents/ui/components/alert";
import { cn } from "@gents/ui/lib/utils";
import { Disclosure } from "./Disclosure";

export type NoticeTone = "info" | "warning" | "destructive";

const ICON = { info: Info, warning: TriangleAlert, destructive: CircleAlert };

const TONE: Record<NoticeTone, { box: string; icon: string }> = {
  info: { box: "border-border/60", icon: "text-muted-foreground" },
  warning: { box: "border-warning/40", icon: "text-warning" },
  destructive: { box: "border-destructive/30", icon: "text-destructive" },
};

/** The app's notice: a title, what to know or do, the person's choices,
    and the raw report folded under Details. Errors and limits use it rather
    than a box of their own, so they read the same everywhere. */
export function Notice({
  tone = "info",
  title,
  icon,
  children,
  actions,
  details,
  detailsLabel = "Details",
  detailsText = false,
  className,
  ...props
}: {
  tone?: NoticeTone;
  title: ReactNode;
  /** in place of the tone's icon, e.g. the provider's logo */
  icon?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  /** the raw report, shown only on request */
  details?: ReactNode;
  detailsLabel?: string;
  /** the details are the person's own words, not a raw report */
  detailsText?: boolean;
} & Omit<ComponentProps<"div">, "title">) {
  const Icon = ICON[tone];
  return (
    <Alert
      role={tone === "info" ? "status" : "alert"}
      {...props}
      className={cn(
        "grid-cols-[auto_minmax(0,1fr)] gap-x-2.5 bg-raised text-foreground",
        TONE[tone].box,
        className,
      )}
    >
      {/* a wrapped icon, so a provider's logo sits where the tone's would */}
      <span
        aria-hidden
        className={cn("row-span-3 mt-0.5 flex size-4 [&_svg]:size-4", TONE[tone].icon)}
      >
        {icon ?? <Icon />}
      </span>
      <AlertTitle className="col-start-2">{title}</AlertTitle>
      {(children || actions || details) && (
        <div className="col-start-2 grid gap-2 text-sm text-muted-foreground">
          {children}
          {details && (
            <Disclosure summary={detailsLabel} summaryClassName="text-xs">
              {detailsText ? (
                <p className="mt-1 max-h-24 overflow-auto text-xs whitespace-pre-wrap break-words">
                  {details}
                </p>
              ) : (
                <pre className="mt-1 max-h-40 overflow-auto font-mono text-[11px] whitespace-pre-wrap break-words text-muted-foreground">
                  {details}
                </pre>
              )}
            </Disclosure>
          )}
          {actions && (
            <div className="flex flex-wrap items-center gap-2 pt-1">{actions}</div>
          )}
        </div>
      )}
    </Alert>
  );
}

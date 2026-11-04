import { useId, useState, type ComponentProps, type ReactNode } from "react";
import { ChevronRight, CircleAlert, Info, TriangleAlert } from "lucide-react";
import { Alert, AlertTitle } from "@gents/ui/components/alert";
import { cn } from "@gents/ui/lib/utils";

export type NoticeTone = "info" | "warning" | "destructive";

const ICON = { info: Info, warning: TriangleAlert, destructive: CircleAlert };

const TONE: Record<NoticeTone, { box: string; icon: string }> = {
  info: { box: "border-border/60", icon: "text-muted-foreground" },
  warning: { box: "border-warning/40", icon: "text-warning" },
  destructive: { box: "border-destructive/30", icon: "text-destructive" },
};

/** The app's notice: a title, a line saying what happened, the person's
    choices, and the raw report a click on that line away. Errors and limits
    use it rather than a box of their own, so they read the same everywhere. */
export function Notice({
  tone = "info",
  title,
  icon,
  description,
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
  /** the line under the title; with details, it opens them */
  description?: ReactNode;
  children?: ReactNode;
  actions?: ReactNode;
  /** the raw report, shown only on request */
  details?: ReactNode;
  /** the line that opens the details when there is no description */
  detailsLabel?: string;
  /** the details are the person's own words, not a raw report */
  detailsText?: boolean;
} & Omit<ComponentProps<"div">, "title">) {
  const Icon = ICON[tone];
  const [open, setOpen] = useState(false);
  const detailsId = useId();
  const line = description ?? (details ? detailsLabel : null);
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
      {(line || children || actions) && (
        <div className="col-start-2 grid gap-2 text-sm text-muted-foreground">
          {line &&
            (details ? (
              <button
                type="button"
                aria-expanded={open}
                aria-controls={detailsId}
                onClick={() => setOpen((was) => !was)}
                className="group/line -mx-1 w-fit rounded-sm px-1 text-left outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
              >
                {line}
                <span className="sr-only">
                  {open ? " Hide details" : " Show details"}
                </span>
                <ChevronRight
                  aria-hidden
                  className={cn(
                    "ml-1 inline size-3.5 align-[-2px] transition-transform",
                    open && "rotate-90",
                  )}
                />
              </button>
            ) : (
              <p>{line}</p>
            ))}
          {/* kept in the document while closed, so its text can be found */}
          {details && (
            <div id={detailsId} hidden={!open}>
              {detailsText ? (
                <p className="max-h-24 overflow-auto text-xs whitespace-pre-wrap break-words">
                  {details}
                </p>
              ) : (
                <pre className="max-h-40 overflow-auto font-mono text-[11px] whitespace-pre-wrap break-words text-muted-foreground">
                  {details}
                </pre>
              )}
            </div>
          )}
          {children}
          {actions && (
            <div className="flex flex-wrap items-center gap-2 pt-1">{actions}</div>
          )}
        </div>
      )}
    </Alert>
  );
}

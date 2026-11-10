import type { ReactNode } from "react";
import { ChevronDown, Download } from "lucide-react";
import { Skeleton } from "@gents/ui/components/skeleton";
import { cn } from "@gents/ui/lib/utils";
import { initials, namespaceOf, shortName } from "./model";

/** A pack's mark: its initials on a quiet tile, where it has no picture. */
export function PackMark({ pack, large = false }: { pack: string; large?: boolean }) {
  return (
    <span
      aria-hidden
      className={cn(
        "grid shrink-0 place-items-center bg-raised font-mono font-medium text-muted-foreground",
        large ? "size-12 rounded-xl text-sm" : "size-9 rounded-lg text-xs",
      )}
    >
      {initials(pack)}
    </span>
  );
}

/** what a pack adds, as quiet chips */
export function Chips({ items }: { items: readonly string[] }) {
  if (!items.length) return null;
  return (
    <span className="flex flex-wrap gap-1">
      {items.map((item) => (
        <span
          key={item}
          className="rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground"
        >
          {item}
        </span>
      ))}
    </span>
  );
}

/* the row is as wide as the page's heading and its text starts where the
   heading's does; only its hover reaches past the edges, so it has room */
export const HOVER_OUT =
  "isolate before:pointer-events-none before:absolute before:inset-y-0 before:-inset-x-3 before:-z-10 before:rounded-lg before:transition-colors";
export const CARD = `group/row relative flex min-w-0 items-center gap-3 py-2.5 text-left ${HOVER_OUT} hover:before:bg-accent has-[a:focus-visible]:before:ring-2 has-[a:focus-visible]:before:ring-ring/50`;

/** Rows shaped like packs while the list is read, so it doesn't jump when
    they arrive. */
export function PackRowsLoading({ label, rows = 3 }: { label: string; rows?: number }) {
  return (
    <ul aria-label={label} aria-busy="true" className="divide-y divide-border/60">
      {Array.from({ length: rows }, (_, i) => (
        <li key={i} className="flex items-center gap-3 py-2.5">
          <Skeleton className="size-9 rounded-lg" />
          <span className="grid flex-1 gap-1.5">
            <Skeleton className="h-3.5 w-40 max-w-full" />
            <Skeleton className="h-3 w-72 max-w-full" />
          </span>
          <Skeleton className="h-3 w-12" />
        </li>
      ))}
    </ul>
  );
}

const compact = new Intl.NumberFormat(undefined, { notation: "compact" });

/** A pack's keywords and how often it's been downloaded, as the registry
    list in the configuration shows them. */
export function PackMeta({
  keywords,
  downloads,
  className,
}: {
  keywords: readonly string[];
  downloads: number | null;
  className?: string;
}) {
  if (!keywords.length && downloads === null) return null;
  return (
    <span className={cn("flex flex-wrap items-center gap-1.5", className)}>
      {keywords.map((word) => (
        <span
          key={word}
          className="rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground"
        >
          {word}
        </span>
      ))}
      {downloads !== null && (
        <span
          title={`${downloads.toLocaleString()} downloads`}
          className="inline-flex items-center gap-1 text-xs text-muted-foreground tabular-nums"
        >
          <Download aria-hidden className="size-3" />
          <span className="sr-only">Downloads: </span>
          {compact.format(downloads)}
        </span>
      )}
    </span>
  );
}

/** The quiet row that ends a list with more to show: Show more, or
    Browse the registry. */
export function MoreRow({
  children,
  onClick,
}: {
  children: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`relative flex cursor-pointer items-center gap-1.5 py-2.5 text-left text-sm text-muted-foreground transition-colors outline-none hover:text-foreground ${HOVER_OUT} hover:before:bg-accent focus-visible:before:ring-2 focus-visible:before:ring-ring/50`}
    >
      {children}
      <ChevronDown aria-hidden className="size-3.5" />
    </button>
  );
}

/** A list of packs, divided like the sessions list. */
export function PackList({ label, children }: { label?: string; children: ReactNode }) {
  return (
    <ul aria-label={label} className="divide-y divide-border/60">
      {children}
    </ul>
  );
}

/** One pack as a row of the list: its mark, name and namespace, a line
    about it, what it adds and how it stands. The name is a link stretched
    over the row, so the whole row opens the pack while `action` stays its
    own control. */
export function PackCard({
  pack,
  to,
  description,
  adds,
  status,
  warn = false,
  action,
  menu,
  meta,
  badge,
}: {
  pack: string;
  to: string;
  description: string | null;
  adds: readonly string[];
  status: ReactNode;
  warn?: boolean;
  action?: ReactNode;
  /** the row's menu, shown while it's pointed at or open */
  menu?: ReactNode;
  /** what the registry says of it at a glance, where there's room */
  meta?: ReactNode;
  /** a word beside its name, such as Yours */
  badge?: ReactNode;
}) {
  const namespace = namespaceOf(pack);
  return (
    <li className={CARD} data-testid="pack-card">
      <PackMark pack={pack} />
      <span className="grid min-w-0 flex-1 gap-0.5">
        <span className="flex min-w-0 items-baseline gap-2">
          <a
            href={to}
            className="truncate text-sm outline-none after:absolute after:inset-0 after:rounded-lg"
          >
            {shortName(pack)}
          </a>
          {namespace && (
            <span className="shrink-0 text-xs text-muted-foreground">{namespace}</span>
          )}
          {badge && (
            <span className="shrink-0 rounded-md bg-muted px-1.5 py-px text-[11px] text-muted-foreground">
              {badge}
            </span>
          )}
        </span>
        <span className="truncate text-xs text-muted-foreground">
          {description ?? adds.join(" · ")}
        </span>
      </span>
      {description && adds.length > 0 && (
        <span className="hidden shrink-0 md:block">
          <Chips items={adds} />
        </span>
      )}
      {meta && <span className="hidden shrink-0 lg:block">{meta}</span>}
      <span
        className={cn(
          "max-w-48 min-w-10 shrink-0 truncate text-right text-xs tabular-nums",
          warn ? "text-warning" : "text-muted-foreground",
        )}
      >
        {status}
      </span>
      {action && (
        <span className="relative z-10 flex min-w-20 shrink-0 justify-end">
          {action}
        </span>
      )}
      {menu && (
        <span className="relative z-10 shrink-0 opacity-0 group-hover/row:opacity-100 focus-within:opacity-100 has-[[data-popup-open]]:opacity-100 [@media(hover:none)]:opacity-100">
          {menu}
        </span>
      )}
    </li>
  );
}

/** a section of the page: a quiet heading over its content */
export function Section({
  title,
  action,
  children,
}: {
  title: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section aria-label={title} className="grid gap-3">
      <div className="flex items-center gap-2">
        <h2 className="flex-1 font-mono text-[11px] tracking-wide text-muted-foreground uppercase">
          {title}
        </h2>
        {action}
      </div>
      {children}
    </section>
  );
}

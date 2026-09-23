/* A section that is a list of documents and one document's settings,
   routed by item id, as the desktop app's config tabs are (list on the
   left, editor beside it; here the list is the page and a row opens the
   editor). */
import { Fragment, useMemo, useRef, useState, type ReactNode } from "react";
import { ArrowLeft, ChevronDown, Plus, Trash2 } from "lucide-react";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import { Checkbox } from "@gents/ui/components/checkbox";
import { FieldLegend, FieldRows, FieldSet } from "@gents/ui/components/field";
import { toast } from "sonner";
import { href, navigate, type Route } from "@/lib/router";
import { Row } from "./rows";

/* a row is a link to its document, or a button when it acts instead */
function RowLink({
  onOpen,
  href,
  className,
  children,
}: {
  onOpen?: () => void;
  href: string;
  className: string;
  children: ReactNode;
}) {
  return onOpen ? (
    <button type="button" onClick={onOpen} className={className}>
      {children}
    </button>
  ) : (
    <a href={href} className={className}>
      {children}
    </a>
  );
}

/* one row; nested rows sit under their parent, indented, on the ground */
function ListItem({
  row: r,
  base,
  nested = false,
  compact = false,
  expanded,
  onToggle,
}: {
  row: ListRow;
  base: Extract<Route, { name: "agent" }>;
  nested?: boolean;
  /* the one-line layout at the list's own indent */
  compact?: boolean;
  /* set when the row has nested rows: whether they are shown */
  expanded?: boolean;
  onToggle?: () => void;
}) {
  /* nested rows are one line each: name, then what it is, in the parent's shade */
  if (nested || compact) {
    return (
      <li
        className={`flex items-center hover:bg-accent ${nested ? "bg-background/40" : ""}`}
      >
        <RowLink
          onOpen={r.onOpen}
          href={r.href ?? href({ ...base, item: r.id })}
          className={`flex min-w-0 flex-1 items-center gap-2 py-1.5 text-left ${nested ? "pl-10" : "pl-4"}`}
        >
          {/* nested: a 16px slot before the text, so names line up with the parent's title */}
          {nested && (
            <span className="grid size-4 shrink-0 place-items-center">{r.icon}</span>
          )}
          <span className="truncate text-sm">{r.title}</span>
          {r.meta && (
            <span className="truncate text-xs text-muted-foreground">{r.meta}</span>
          )}
          {r.badge && (
            <Badge
              variant={r.badgeTone === "bad" ? "destructive" : "secondary"}
              className="ml-auto shrink-0"
            >
              {r.badge}
            </Badge>
          )}
        </RowLink>
        <div className="flex shrink-0 items-center gap-1 pr-3 pl-2">{r.trailing}</div>
      </li>
    );
  }
  return (
    <li className="flex items-center hover:bg-accent">
      <RowLink
        onOpen={r.onOpen}
        href={r.href ?? href({ ...base, item: r.id })}
        className="grid min-w-0 flex-1 grid-cols-[auto_1fr_auto] items-center gap-4 py-3 pl-4 text-left"
      >
        <span className="grid size-8 place-items-center rounded-lg border border-border/60 bg-background">
          {r.icon ?? <span className="size-2 rounded-full bg-border" />}
        </span>
        <div className="min-w-0">
          <p className="flex min-w-0 items-baseline gap-2">
            <span className="truncate text-sm font-medium">{r.title}</span>
            {r.titleNote && (
              <span className="shrink-0 font-mono text-[10px] tracking-wider text-ink uppercase">
                {r.titleNote}
              </span>
            )}
          </p>
          {(r.meta || packOrigin(r.tags)) && (
            <p
              className={`mt-0.5 truncate text-muted-foreground ${r.metaMono ? "font-mono text-[11px]" : "text-xs"}`}
            >
              {r.metaLeadToggles && onToggle && r.meta ? (
                <>
                  <button
                    type="button"
                    className="underline-offset-2 hover:text-foreground hover:underline"
                    onClick={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      onToggle();
                    }}
                  >
                    {r.meta.split(" · ")[0]}
                  </button>
                  {r.meta.includes(" · ")
                    ? ` · ${r.meta.split(" · ").slice(1).join(" · ")}`
                    : ""}
                </>
              ) : (
                r.meta
              )}
              {packOrigin(r.tags)
                ? `${r.meta ? " · " : ""}pack ${packOrigin(r.tags)}`
                : ""}
            </p>
          )}
        </div>
        {r.badge && (
          <Badge variant={r.badgeTone === "bad" ? "destructive" : "secondary"}>
            {r.badge}
          </Badge>
        )}
      </RowLink>
      <div className="flex shrink-0 items-center gap-1 pr-3 pl-2">
        {r.trailing}
        {expanded !== undefined && (
          <Button
            variant="quiet"
            size="icon-sm"
            aria-expanded={expanded}
            aria-label={
              expanded ? `Hide what ${r.title} serves` : `Show what ${r.title} serves`
            }
            onClick={onToggle}
          >
            <ChevronDown
              className={`transition-transform ${expanded ? "rotate-180" : ""}`}
            />
          </Button>
        )}
      </div>
    </li>
  );
}

/* past this many rows a list gets a filter */
const FILTER_AFTER = 8;
/* nested rows shown before a Show more row */
const NESTED_SHOWN = 5;

export type ListRow = {
  id: string;
  title: string;
  /* a word beside the title for the one row that stands apart, such as the default */
  titleNote?: string;
  meta?: string;
  badge?: string;
  badgeTone?: "default" | "bad";
  /* a summary of identifiers and modes, set in mono */
  metaMono?: boolean;
  icon?: ReactNode;
  /* controls at the end of the row, outside its link */
  trailing?: ReactNode;
  /* the document's tags, for the pack-origin filter */
  tags?: string[] | null;
  /* a row that acts instead of opening a document (a catalog entry) */
  onOpen?: () => void;
  /* where the row goes when not the section's own document */
  href?: string;
  /* rows that belong to this one (a backend's models), indented beneath it */
  children?: ListRow[];
  /* the first part of meta names the nested rows ("4 models"): clicking it shows them */
  metaLeadToggles?: boolean;
};

/* pack provenance (desktop #1511): a document from a pack carries a
   gents:pack:<origin> tag; lists can filter by it */
const PACK_ORIGIN_PREFIX = "gents:pack:";
const NOT_FROM_PACK_FILTER = "__not_from_pack__";
export function packOrigin(tags: string[] | null | undefined): string | null {
  const origins = (tags ?? [])
    .filter((tag) => tag.startsWith(PACK_ORIGIN_PREFIX))
    .map((tag) => tag.slice(PACK_ORIGIN_PREFIX.length))
    .filter(Boolean);
  return origins.length === 1 ? origins[0] : null;
}

export function ListDetail({
  base,
  item,
  rows,
  onCreate,
  createLabel,
  empty,
  detail,
  back,
  toolbar,
  createMenu,
  compact = false,
}: {
  base: Extract<Route, { name: "agent" }>;
  item?: string;
  rows: ListRow[];
  onCreate?: () => void | Promise<void>;
  createLabel: string;
  empty: string;
  /* renders the open document's editor */
  detail: (id: string) => ReactNode;
  /* where Back goes from the editor; the list when absent */
  back?: { route: Route; label: string };
  /* controls for the open document, at the top right beside Back */
  toolbar?: (id: string) => ReactNode;
  /* a create control of the panel's own (a menu of kinds), instead of the plain button */
  createMenu?: ReactNode;
  /* one line per row: name, then what it is; for lists of small documents */
  compact?: boolean;
}) {
  const creatingRef = useRef(false);
  const [creating, setCreating] = useState(false);
  const [query, setQuery] = useState("");
  const [originFilter, setOriginFilter] = useState("all");
  /* rows whose nested rows are shown; closed by default */
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  /* rows whose nested rows are all shown, past the first few */
  const [allNested, setAllNested] = useState<Set<string>>(() => new Set());
  const origins = useMemo(
    () =>
      Array.from(
        new Set(
          rows
            .map((row) => packOrigin(row.tags))
            .filter((o): o is string => o !== null),
        ),
      ).sort(),
    [rows],
  );
  const effectiveOrigin =
    originFilter === "all" ||
    originFilter === NOT_FROM_PACK_FILTER ||
    origins.includes(originFilter)
      ? originFilter
      : "all";
  const create = async () => {
    if (!onCreate || creatingRef.current) return;
    creatingRef.current = true;
    setCreating(true);
    try {
      await onCreate();
    } catch (error) {
      toast(`Create failed: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      creatingRef.current = false;
      setCreating(false);
    }
  };
  if (item && rows.some((r) => r.id === item)) {
    return (
      <div>
        <div className="mb-6 flex items-center justify-between gap-3">
          <a
            href={href(back?.route ?? { ...base, item: undefined })}
            className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> {back?.label ?? "Back"}
          </a>
          {toolbar?.(item)}
        </div>
        {detail(item)}
      </div>
    );
  }
  const filterable = rows.length > FILTER_AFTER;
  const q = query.trim().toLowerCase();
  const shown = rows.filter((r) => {
    const origin = packOrigin(r.tags);
    const matchesOrigin =
      effectiveOrigin === "all" ||
      (effectiveOrigin === NOT_FROM_PACK_FILTER
        ? origin === null
        : origin === effectiveOrigin);
    const matchesQuery =
      !filterable ||
      !q ||
      [r.title, r.titleNote, r.meta, r.badge].some((t) => t?.toLowerCase().includes(q));
    return matchesOrigin && matchesQuery;
  });
  return (
    <div>
      {(onCreate || createMenu || filterable || origins.length > 0) && (
        <div className="mb-6 flex flex-wrap items-center gap-3">
          {filterable && (
            <Input
              type="search"
              aria-label="Filter the list"
              placeholder={`Filter ${rows.length}`}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              className="w-72 max-md:w-full"
            />
          )}
          {origins.length > 0 && (
            <Select
              items={[
                { value: "all", label: "All origins" },
                ...origins.map((o) => ({ value: o, label: `Pack ${o}` })),
                { value: NOT_FROM_PACK_FILTER, label: "Not from a pack" },
              ]}
              value={effectiveOrigin}
              onValueChange={(v) => setOriginFilter(v ?? "all")}
            >
              <SelectTrigger
                aria-label="Origin"
                data-testid={`${base.section}-origin-filter`}
                className="w-48 max-md:w-full"
              >
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All origins</SelectItem>
                {origins.map((o) => (
                  <SelectItem key={o} value={o}>
                    Pack {o}
                  </SelectItem>
                ))}
                <SelectItem value={NOT_FROM_PACK_FILTER}>Not from a pack</SelectItem>
              </SelectContent>
            </Select>
          )}
          {createMenu && <div className="ml-auto">{createMenu}</div>}
          {onCreate && (
            <Button
              className="ml-auto"
              variant="outline"
              disabled={creating}
              onClick={create}
            >
              <Plus /> {creating ? "Creating…" : createLabel}
            </Button>
          )}
        </div>
      )}
      <ul className="divide-y divide-border/60 rounded-2xl border border-border/60 bg-raised">
        {shown.map((r) => (
          <Fragment key={r.id}>
            <ListItem
              row={r}
              base={base}
              compact={compact}
              expanded={r.children ? expanded.has(r.id) : undefined}
              onToggle={() =>
                setExpanded((prev) => {
                  const next = new Set(prev);
                  if (next.has(r.id)) next.delete(r.id);
                  else next.add(r.id);
                  return next;
                })
              }
            />
            {expanded.has(r.id) &&
              (allNested.has(r.id) || (r.children?.length ?? 0) <= NESTED_SHOWN + 1
                ? r.children
                : r.children?.slice(0, NESTED_SHOWN)
              )?.map((c) => <ListItem key={c.id} row={c} base={base} nested />)}
            {expanded.has(r.id) &&
              !allNested.has(r.id) &&
              (r.children?.length ?? 0) > NESTED_SHOWN + 1 && (
                <ListItem
                  base={base}
                  nested
                  row={{
                    id: `${r.id}:more`,
                    title: `Show ${(r.children?.length ?? 0) - NESTED_SHOWN} more`,
                    onOpen: () => setAllNested((prev) => new Set(prev).add(r.id)),
                  }}
                />
              )}
          </Fragment>
        ))}
        {rows.length === 0 && (
          <li className="px-4 py-8 text-center text-sm text-muted-foreground">
            {empty}
          </li>
        )}
        {rows.length > 0 && shown.length === 0 && (
          <li className="px-4 py-8 text-center text-sm text-muted-foreground">
            {q ? `Nothing matches “${query.trim()}”.` : "Nothing from that origin."}
          </li>
        )}
      </ul>
      {filterable && q && shown.length > 0 && (
        <p className="mt-2 text-xs text-muted-foreground">
          {shown.length} of {rows.length}
        </p>
      )}
    </div>
  );
}

/* what a section's documents are called, for the delete wording */
export const NOUNS: Record<string, string> = {
  behaviors: "behavior",
  contexts: "context",
  skills: "skill",
  inference: "backend",
  profiles: "inference profile",
  tools: "Tools document",
  "tool-services": "tool service",
  tasks: "task",
  schedules: "schedule",
  "event-sources": "event source",
  triggers: "trigger",
  automations: "automation",
};

/* The typed-name confirmation every delete goes through, from the editor's
   Danger zone or a list row's menu. The name is always asked for; `warning`
   says which documents lose their reference (see dependents.ts). */
export function ConfirmDelete({
  label,
  noun,
  open,
  onOpenChange,
  onDelete,
  after,
  warning,
  companion,
}: {
  label: string;
  noun: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onDelete: () => Promise<unknown>;
  /* where to go once deleted; stays put when absent */
  after?: Route;
  /* what else the delete leaves behind, said in the confirmation */
  warning?: string;
  /* a document that can go with it, offered as a ticked box */
  companion?: { label: string; onDelete: () => Promise<unknown> };
}) {
  const [typed, setTyped] = useState("");
  const [deleting, setDeleting] = useState(false);
  const [alsoDelete, setAlsoDelete] = useState(true);
  const confirmed = typed.trim() === label.trim();
  const close = (next: boolean) => {
    if (deleting) return;
    onOpenChange(next);
    if (!next) {
      setTyped("");
      setAlsoDelete(true);
    }
  };
  async function remove() {
    if (!confirmed || deleting) return;
    setDeleting(true);
    try {
      await onDelete();
      const withCompanion = companion !== undefined && alsoDelete;
      if (withCompanion) await companion.onDelete();
      toast(
        withCompanion
          ? `Deleted ${label} and ${companion.label.replace(/^Also delete /, "")}`
          : `Deleted ${label}`,
      );
      setTyped("");
      onOpenChange(false);
      if (after) navigate(after);
    } catch (error) {
      toast(`Delete failed: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setDeleting(false);
    }
  }
  return (
    <AlertDialog open={open} onOpenChange={close}>
      <AlertDialogContent aria-modal="true">
        <AlertDialogHeader>
          <AlertDialogTitle>Delete {label}?</AlertDialogTitle>
          <AlertDialogDescription>
            This removes the {noun} for good.{warning ? ` ${warning}` : ""} To confirm,
            type its name: <span className="font-medium text-foreground">{label}</span>
          </AlertDialogDescription>
        </AlertDialogHeader>
        <Input
          autoFocus
          aria-label={`Type ${label} to confirm`}
          value={typed}
          onChange={(e) => setTyped(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void remove();
          }}
        />
        {companion && (
          <label className="flex items-center gap-2 text-sm">
            <Checkbox
              checked={alsoDelete}
              onCheckedChange={(v) => setAlsoDelete(Boolean(v))}
            />
            {companion.label}
          </label>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <Button
            variant="destructive"
            disabled={!confirmed || deleting}
            onClick={() => void remove()}
          >
            {deleting ? "Deleting…" : `Delete ${noun}`}
          </Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

/* The end of an editor: a Danger zone group, set apart from the fields and
   their Save. Its delete goes through ConfirmDelete, which always asks for
   the document's name before it runs. */
export function DeleteButton({
  label,
  onDelete,
  base,
  after,
  warning,
  companion,
}: {
  label: string;
  onDelete: () => Promise<unknown>;
  base: Extract<Route, { name: "agent" }>;
  /* where to go once deleted; the list when absent */
  after?: Route;
  /* what else the delete leaves behind, said in the confirmation */
  warning?: string;
  /* a document that can go with it, offered as a ticked box */
  companion?: { label: string; onDelete: () => Promise<unknown> };
}) {
  const noun = NOUNS[base.section] ?? "item";
  const [open, setOpen] = useState(false);
  return (
    <FieldSet className="mt-12 mb-8" data-testid="danger-zone">
      <FieldLegend variant="eyebrow">Danger zone</FieldLegend>
      <FieldRows className="ring-destructive/25 dark:ring-destructive/30">
        <Row
          label={`Delete this ${noun}`}
          description="Removes it for good. This cannot be undone."
        >
          <Button
            variant="outline"
            className="text-destructive hover:text-destructive"
            onClick={() => setOpen(true)}
          >
            <Trash2 /> Delete {noun}
          </Button>
        </Row>
      </FieldRows>
      <ConfirmDelete
        label={label}
        noun={noun}
        open={open}
        onOpenChange={setOpen}
        onDelete={onDelete}
        after={after ?? { ...base, item: undefined }}
        warning={warning}
        companion={companion}
      />
    </FieldSet>
  );
}

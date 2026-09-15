/* A section that is a list of documents and one document's settings,
   routed by item id, as the desktop app's config tabs are (list on the
   left, editor beside it; here the list is the page and a row opens the
   editor). */
import { useMemo, useRef, useState, type ReactNode } from "react";
import { ArrowLeft, Plus, Trash2 } from "lucide-react";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { FieldLegend, FieldRows, FieldSet } from "@gents/ui/components/field";
import { Input } from "@gents/ui/components/input";
import { toast } from "sonner";
import { href, navigate, type Route } from "@/lib/router";
import { Row } from "./rows";

export type ListRow = {
  id: string;
  title: string;
  meta?: string;
  badge?: string;
  badgeTone?: "default" | "bad";
  icon?: ReactNode;
  tags?: string[] | null;
};

const PACK_ORIGIN_PREFIX = "gents:pack:";
const NOT_FROM_PACK_FILTER = "__not_from_pack__";
const FILTER_AFTER = 8;

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
}: {
  base: Extract<Route, { name: "agent" }>;
  item?: string;
  rows: ListRow[];
  onCreate?: () => void | Promise<void>;
  createLabel: string;
  empty: string;
  /* renders the open document's editor */
  detail: (id: string) => ReactNode;
}) {
  const creatingRef = useRef(false);
  const [creating, setCreating] = useState(false);
  const [originFilter, setOriginFilter] = useState("all");
  const [query, setQuery] = useState("");
  const origins = useMemo(
    () =>
      Array.from(
        new Set(
          rows
            .map((row) => packOrigin(row.tags))
            .filter((origin): origin is string => origin !== null),
        ),
      ).sort(),
    [rows],
  );
  const effectiveOriginFilter =
    originFilter === "all" ||
    originFilter === NOT_FROM_PACK_FILTER ||
    origins.includes(originFilter)
      ? originFilter
      : "all";
  const filterable = rows.length > FILTER_AFTER;
  const normalizedQuery = query.trim().toLowerCase();
  const visibleRows = rows.filter((row) => {
    const origin = packOrigin(row.tags);
    const matchesOrigin =
      effectiveOriginFilter === "all" ||
      (effectiveOriginFilter === NOT_FROM_PACK_FILTER
        ? origin === null
        : origin === effectiveOriginFilter);
    const matchesQuery =
      !filterable ||
      !normalizedQuery ||
      [row.title, row.meta, row.badge].some((text) =>
        text?.toLowerCase().includes(normalizedQuery),
      );
    return matchesOrigin && matchesQuery;
  });
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
        <a
          href={href({ ...base, item: undefined })}
          className="mb-6 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="size-3.5" /> Back
        </a>
        {detail(item)}
      </div>
    );
  }
  return (
    <div>
      {(onCreate || origins.length > 0 || filterable) && (
        <div className="mb-6 flex flex-wrap items-end gap-3">
          {filterable && (
            <Input
              type="search"
              aria-label="Filter the list"
              placeholder={`Filter ${rows.length}`}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              className="w-72 max-md:w-full"
            />
          )}
          {origins.length > 0 ? (
            <label className="grid gap-1 text-xs text-muted-foreground">
              Origin
              <select
                className="rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground"
                data-testid={`${base.section}-origin-filter`}
                value={effectiveOriginFilter}
                onChange={(event) => setOriginFilter(event.target.value)}
              >
                <option value="all">All</option>
                {origins.map((origin) => (
                  <option key={origin} value={origin}>
                    {origin}
                  </option>
                ))}
                <option value={NOT_FROM_PACK_FILTER}>Not from a pack</option>
              </select>
            </label>
          ) : null}
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
        {visibleRows.map((r) => {
          const origin = packOrigin(r.tags);
          return (
            <li key={r.id}>
              <a
                href={href({ ...base, item: r.id })}
                className="grid grid-cols-[auto_1fr_auto] items-center gap-4 px-4 py-3 hover:bg-accent"
              >
                <span className="grid size-8 place-items-center rounded-lg border border-border/60 bg-background">
                  {r.icon ?? <span className="size-2 rounded-full bg-border" />}
                </span>
                <div className="min-w-0">
                  <p className="truncate text-sm font-medium">{r.title}</p>
                  {r.meta && (
                    <p className="mt-0.5 truncate text-xs text-muted-foreground">
                      {r.meta}
                      {origin ? ` · pack:${origin}` : ""}
                    </p>
                  )}
                </div>
                {r.badge && (
                  <Badge variant={r.badgeTone === "bad" ? "destructive" : "secondary"}>
                    {r.badge}
                  </Badge>
                )}
              </a>
            </li>
          );
        })}
        {visibleRows.length === 0 && (
          <li className="px-4 py-8 text-center text-sm text-muted-foreground">
            {rows.length === 0 ? empty : "No matching documents."}
          </li>
        )}
      </ul>
      {filterable && normalizedQuery && visibleRows.length > 0 && (
        <p className="mt-2 text-xs text-muted-foreground">
          {visibleRows.length} of {rows.length}
        </p>
      )}
    </div>
  );
}

const NOUNS: Record<string, string> = {
  behaviors: "behaviour",
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
};

export function DeleteButton({
  label,
  onDelete,
  base,
  warning,
}: {
  label: string;
  onDelete: () => Promise<unknown>;
  base: Extract<Route, { name: "agent" }>;
  warning?: string;
}) {
  const noun = NOUNS[base.section] ?? "item";
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState("");
  const [deleting, setDeleting] = useState(false);
  const confirmed = typed.trim() === label.trim();
  const close = (next: boolean) => {
    if (deleting) return;
    setOpen(next);
    if (!next) setTyped("");
  };

  async function remove() {
    if (!confirmed || deleting) return;
    setDeleting(true);
    try {
      await onDelete();
      toast(`Deleted ${label}`);
      setOpen(false);
      navigate({ ...base, item: undefined });
    } catch (error) {
      toast(`Delete failed: ${error instanceof Error ? error.message : String(error)}`);
    } finally {
      setDeleting(false);
    }
  }

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
      <AlertDialog open={open} onOpenChange={close}>
        <AlertDialogContent aria-modal="true">
          <AlertDialogHeader>
            <AlertDialogTitle>Delete {label}?</AlertDialogTitle>
            <AlertDialogDescription>
              This removes the {noun} for good.{warning ? ` ${warning}` : ""} To
              confirm, type its name:{" "}
              <span className="font-medium text-foreground">{label}</span>
            </AlertDialogDescription>
          </AlertDialogHeader>
          <Input
            autoFocus
            aria-label={`Type ${label} to confirm`}
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") void remove();
            }}
          />
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
    </FieldSet>
  );
}

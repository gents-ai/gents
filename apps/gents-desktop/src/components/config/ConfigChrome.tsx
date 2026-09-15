import { useMemo, useState } from "react";
import type React from "react";
import {
  useConfigNavigationGuard,
  useReportConfigDirty,
} from "./ConfigNavigationGuard";

export type ConfigEditorHeaderProps = {
  eyebrow: string;
  saved: boolean;
  title: string;
  dirty?: boolean;
};

export function ConfigEditorHeader({
  eyebrow,
  saved,
  title,
  dirty = false,
}: ConfigEditorHeaderProps) {
  return (
    <div className="panel-header">
      <div>
        <p className="eyebrow">{eyebrow}</p>
        <h3>{title}</h3>
      </div>
      <EditorStatusChip dirty={dirty} saved={saved} />
    </div>
  );
}

export function EditorStatusChip({ dirty, saved }: { dirty: boolean; saved: boolean }) {
  useReportConfigDirty(dirty);

  if (dirty) {
    return (
      <span className="chip chip-amber" data-testid="unsaved-chip">
        Unsaved changes
      </span>
    );
  }
  if (saved) {
    return <span className="chip chip-green">Saved</span>;
  }
  return null;
}

export function FieldHint({
  show,
  children,
}: {
  show: boolean;
  children: React.ReactNode;
}) {
  if (!show) {
    return null;
  }
  return (
    <span className="field-hint-error" role="alert">
      {children}
    </span>
  );
}

export type ConfigDocumentListItem = {
  id: string;
  title: string;
  meta: string;
  tags?: string[] | null;
};

const PACK_ORIGIN_PREFIX = "gents:pack:";
const NOT_FROM_PACK_FILTER = "__not_from_pack__";

export function packOrigin(tags: string[] | null | undefined): string | null {
  const origins = (tags ?? [])
    .filter((tag) => tag.startsWith(PACK_ORIGIN_PREFIX))
    .map((tag) => tag.slice(PACK_ORIGIN_PREFIX.length))
    .filter(Boolean);
  return origins.length === 1 ? origins[0] : null;
}

export type ConfigDocumentListProps = {
  createLabel?: string;
  eyebrow: string;
  items: ConfigDocumentListItem[];
  selectedId: string | null;
  testPrefix: string;
  title: string;
  onCreate?: () => void;
  onSelect: (id: string) => void;
};

export function ConfigDocumentList({
  createLabel = "Add New",
  eyebrow,
  items,
  selectedId,
  testPrefix,
  title,
  onCreate,
  onSelect,
}: ConfigDocumentListProps) {
  const { requestNavigation } = useConfigNavigationGuard();
  const [originFilter, setOriginFilter] = useState("all");
  const origins = useMemo(
    () =>
      Array.from(
        new Set(
          items
            .map((item) => packOrigin(item.tags))
            .filter((origin): origin is string => origin !== null),
        ),
      ).sort(),
    [items],
  );
  const effectiveOriginFilter =
    originFilter === "all" ||
    originFilter === NOT_FROM_PACK_FILTER ||
    origins.includes(originFilter)
      ? originFilter
      : "all";
  const filteredItems = items.filter((item) => {
    const origin = packOrigin(item.tags);
    return (
      effectiveOriginFilter === "all" ||
      (effectiveOriginFilter === NOT_FROM_PACK_FILTER
        ? origin === null
        : origin === effectiveOriginFilter)
    );
  });

  return (
    <aside className="panel config-list config-document-list">
      <div className="config-list-controls">
        <div className="panel-header">
          <div>
            <p className="eyebrow">{eyebrow}</p>
            <h3>{title}</h3>
          </div>
          {onCreate ? (
            <button
              className="ghost-button"
              data-testid={`${testPrefix}-new`}
              onClick={() => requestNavigation(onCreate)}
              type="button"
            >
              {createLabel}
            </button>
          ) : null}
        </div>
        {origins.length ? (
          <label className="config-origin-filter">
            <span>Origin</span>
            <select
              data-testid={`${testPrefix}-origin-filter`}
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
      </div>
      <div className="config-document-list-body">
        {filteredItems.map((item) => {
          const origin = packOrigin(item.tags);
          return (
            <button
              className={item.id === selectedId ? "list-item selected" : "list-item"}
              data-testid={`config-${testPrefix}-${item.id}`}
              key={item.id}
              onClick={() => {
                if (item.id === selectedId) {
                  onSelect(item.id);
                  return;
                }
                requestNavigation(() => onSelect(item.id));
              }}
              type="button"
            >
              <span className="list-item-title">{item.title}</span>
              <span className="list-item-meta">
                {item.meta}
                {origin ? ` · pack:${origin}` : ""}
              </span>
            </button>
          );
        })}
        {!filteredItems.length ? <p className="muted">No matching documents.</p> : null}
      </div>
    </aside>
  );
}

export function PencilIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path d="M12 20h9" />
      <path d="m16.5 3.5 4 4L7 21H3v-4L16.5 3.5Z" />
    </svg>
  );
}

export function PlusIcon() {
  return (
    <svg aria-hidden="true" viewBox="0 0 24 24">
      <path d="M12 5v14" />
      <path d="M5 12h14" />
    </svg>
  );
}

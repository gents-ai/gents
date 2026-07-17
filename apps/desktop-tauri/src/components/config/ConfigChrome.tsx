import type React from "react";

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

/** One save-state vocabulary for every editor, including the custom-header
 * Agent/Behavior panels: dirty outranks saved. */
export function EditorStatusChip({ dirty, saved }: { dirty: boolean; saved: boolean }) {
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

/** Inline validation reason — invalid input must explain itself, not just
 * disable Save. */
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
};

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
  return (
    <aside className="panel config-list config-document-list">
      <div className="panel-header">
        <div>
          <p className="eyebrow">{eyebrow}</p>
          <h3>{title}</h3>
        </div>
        {onCreate ? (
          <button
            className="ghost-button"
            data-testid={`${testPrefix}-new`}
            onClick={onCreate}
            type="button"
          >
            {createLabel}
          </button>
        ) : null}
      </div>
      <div className="config-document-list-body">
        {items.map((item) => (
          <button
            className={item.id === selectedId ? "list-item selected" : "list-item"}
            data-testid={`config-${testPrefix}-${item.id}`}
            key={item.id}
            onClick={() => onSelect(item.id)}
            type="button"
          >
            <span className="list-item-title">{item.title}</span>
            <span className="list-item-meta">{item.meta}</span>
          </button>
        ))}
        {!items.length ? <p className="muted">No documents yet.</p> : null}
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

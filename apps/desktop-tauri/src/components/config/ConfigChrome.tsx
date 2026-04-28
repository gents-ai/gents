export type ConfigEditorHeaderProps = {
  eyebrow: string;
  saved: boolean;
  title: string;
};

export function ConfigEditorHeader({
  eyebrow,
  saved,
  title,
}: ConfigEditorHeaderProps) {
  return (
    <div className="panel-header">
      <div>
        <p className="eyebrow">{eyebrow}</p>
        <h3>{title}</h3>
      </div>
      {saved ? <span className="chip chip-green">Saved</span> : null}
    </div>
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

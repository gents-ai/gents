/* Shared configuration editor rows plus explicit Save/Cancel actions.
   Fields only update their local draft; persistence is user-controlled. */
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ExternalLink, FolderOpen, Plus, XIcon } from "lucide-react";
import { canPickDirectory, pickDirectory } from "@/lib/pickDirectory";
import { Input } from "@gents/ui/components/input";
import { Button } from "@gents/ui/components/button";
import {
  Combobox,
  ComboboxChip,
  ComboboxChips,
  ComboboxChipsInput,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxItem,
  ComboboxInput,
  ComboboxList,
  ComboboxTrigger,
  ComboboxValue,
} from "@gents/ui/components/combobox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { href, type Route } from "@/lib/router";
import { Switch } from "@gents/ui/components/switch";
import { Textarea } from "@gents/ui/components/textarea";
import { Fact, Row, StackedRow } from "./rows";

export type Choice = { value: string; label: string; disabled?: boolean };

type Common = {
  id: string;
  label: ReactNode;
  description?: ReactNode;
  /* shown at the field, under its description */
  error?: string;
  disabled?: boolean;
};

export function DraftActions({
  dirty,
  saving,
  error,
  onSave,
  onCancel,
  saveLabel = "Save",
}: {
  dirty: boolean;
  saving: boolean;
  error: string | null;
  onSave: () => unknown;
  onCancel: () => void;
  saveLabel?: string;
}) {
  return (
    <div className="mb-6">
      {error && (
        <p role="alert" className="mb-3 text-sm text-destructive">
          {error}
        </p>
      )}
      <div className="flex justify-end gap-2">
        <Button variant="quiet" disabled={!dirty || saving} onClick={onCancel}>
          Cancel
        </Button>
        <Button
          variant="brand"
          disabled={!dirty || saving}
          onClick={() => void onSave()}
        >
          {saving ? "Saving…" : saveLabel}
        </Button>
      </div>
    </div>
  );
}

export function TextRow({
  id,
  label,
  description,
  value,
  onChange,
  onCommit,
  onEnter,
  placeholder,
  mono,
  password,
  wide,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  onEnter: (e: React.KeyboardEvent<HTMLElement>) => void;
  placeholder?: string;
  mono?: boolean;
  password?: boolean;
  wide?: boolean;
}) {
  return (
    <Row label={label} description={description} htmlFor={id} error={error}>
      <Input
        id={id}
        aria-invalid={error ? true : undefined}
        disabled={disabled}
        type={password ? "password" : "text"}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        onKeyDown={onEnter}
        placeholder={placeholder}
        className={`${wide ? "w-96 max-md:w-full" : "w-72 max-md:w-full"} ${mono ? "font-mono text-xs" : ""}`}
      />
    </Row>
  );
}

/* a number kept as text while editing; empty means null */
export function NumberRow({
  id,
  label,
  description,
  value,
  onChange,
  onCommit,
  onEnter,
  placeholder,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  onEnter: (e: React.KeyboardEvent<HTMLElement>) => void;
  placeholder?: string;
}) {
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Input
        id={id}
        inputMode="decimal"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        onKeyDown={onEnter}
        placeholder={placeholder}
        className="w-36"
      />
    </Row>
  );
}

export function AreaRow({
  id,
  label,
  description,
  value,
  onChange,
  onCommit,
  placeholder,
  rows = 3,
  mono,
  stacked,
  expandedByDefault = false,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  placeholder?: string;
  rows?: number;
  mono?: boolean;
  stacked?: boolean;
  /* a stacked area that opens showing all of its text */
  expandedByDefault?: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [expanded, setExpanded] = useState(expandedByDefault);
  const [overflowing, setOverflowing] = useState(false);
  useEffect(() => {
    const element = ref.current;
    if (!stacked || !element) return;
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() =>
      setOverflowing(element.scrollHeight > element.clientHeight + 1),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [stacked, value, expanded]);
  const clipped = stacked && !expanded;
  const Wrap = stacked ? StackedRow : Row;
  return (
    <Wrap label={label} description={description} htmlFor={id} error={error}>
      <Textarea
        ref={ref}
        id={id}
        aria-invalid={error ? true : undefined}
        readOnly={disabled}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={onCommit}
        placeholder={placeholder}
        rows={rows}
        style={clipped ? { maxHeight: `calc(${rows}lh + 1rem + 2px)` } : undefined}
        className={`${stacked ? "w-full" : "w-96 max-md:w-full"} ${mono ? "font-mono text-xs" : ""}`}
      />
      {stacked && (overflowing || expanded) && (
        <div className="-mt-1 flex justify-end">
          <Button
            variant="quiet"
            size="sm"
            onClick={() => setExpanded((open) => !open)}
          >
            {expanded ? "Show less" : "Show all"}
          </Button>
        </div>
      )}
    </Wrap>
  );
}

export function ChoiceRow({
  id,
  label,
  description,
  value,
  onChange,
  items,
  none,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  items: Choice[];
  none?: string;
}) {
  const all = none ? [{ value: "", label: none }, ...items] : items;
  return (
    <Row label={label} description={description} htmlFor={id} error={error}>
      <Select
        items={all}
        value={value}
        onValueChange={(v) => onChange(v ?? "")}
        disabled={disabled}
      >
        <SelectTrigger
          id={id}
          aria-invalid={error ? true : undefined}
          className="w-72 max-md:w-full"
        >
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {all.map((i) => (
            <SelectItem key={i.value || "∅"} value={i.value}>
              {i.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </Row>
  );
}

/* a directory: typed, or chosen with the OS picker where
   the app has one (the desktop shell). The button appears only then. */
export function PathRow({
  id,
  label,
  description,
  value,
  onChange,
  onCommit,
  onEnter,
  placeholder,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  onEnter: (e: React.KeyboardEvent<HTMLElement>) => void;
  placeholder?: string;
}) {
  const [picking, setPicking] = useState(false);
  const choose = async () => {
    setPicking(true);
    try {
      const picked = await pickDirectory({
        defaultPath: value || null,
        title: String(label),
      });
      if (picked) {
        onChange(picked);
        onCommit();
      }
    } finally {
      setPicking(false);
    }
  };
  return (
    <Row label={label} description={description} htmlFor={id} error={error}>
      <span className="flex w-72 items-center gap-1 max-md:w-full">
        <Input
          id={id}
          aria-invalid={error ? true : undefined}
          disabled={disabled}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onBlur={onCommit}
          onKeyDown={onEnter}
          placeholder={placeholder ?? "~/Projects/…"}
          className="min-w-0 flex-1 font-mono text-xs"
        />
        {canPickDirectory() && (
          <Button
            variant="quiet"
            size="icon"
            aria-label="Choose a folder"
            disabled={disabled || picking}
            onClick={() => void choose()}
          >
            <FolderOpen />
          </Button>
        )}
      </span>
    </Row>
  );
}

/* a field that points at another document. Three affordances, always: pick
   one, create a new one without leaving the page, open the chosen one.
   `onCreate` makes the document (a dialog or a direct save) and resolves
   with its id, or null if the user backed out. */
export function RefRow({
  id,
  label,
  description,
  value,
  onChange,
  items,
  none,
  error,
  disabled,
  createLabel,
  onCreate,
  openRoute,
  onOpen,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  items: Choice[];
  none?: string;
  createLabel: string;
  onCreate?: () => Promise<string | null>;
  openRoute?: (id: string) => Route;
  /* opens the chosen document beside the page instead of navigating to it */
  onOpen?: (id: string) => void;
}) {
  const all = none ? [{ value: "", label: none }, ...items] : items;
  /* the select renders whatever value it is given; the New item is never it */
  const known = all.some((i) => i.value === value);
  /* a reference to a document that no longer exists: shown as such, and the
     field is invalid until another is picked (the desktop's DocumentSelection rule) */
  const shown = known ? all : [...all, { value, label: `${value} (unavailable)` }];
  const invalid = error ? true : !known && value ? true : undefined;
  /* a searchable popup is a dialog: one at a time with the shell's others */
  const popover = useExclusivePopover();
  /* a label reads "Name · summary"; the summary sits beside the name in the list */
  const labelOf = (v: string) => {
    const full = shown.find((i) => i.value === v)?.label ?? v;
    const at = full.indexOf(" · ");
    return at === -1
      ? { name: full, hint: "" }
      : { name: full.slice(0, at), hint: full.slice(at + 3) };
  };
  return (
    <Row
      label={label}
      description={description}
      htmlFor={id}
      error={
        error ??
        (!known && value
          ? "That document no longer exists. Choose another."
          : undefined)
      }
    >
      <span className="flex w-72 items-center gap-1 max-md:w-full">
        <Combobox
          items={shown.map((i) => i.value)}
          value={value}
          onValueChange={(v) => {
            popover.onOpenChange(false);
            onChange(v ?? "");
          }}
          open={popover.open}
          onOpenChange={popover.onOpenChange}
          onOpenChangeComplete={popover.onOpenChangeComplete}
          disabled={disabled}
          itemToStringLabel={(v: string) => labelOf(v).name}
          filter={(v: string, query: string) => {
            const q = query.trim().toLowerCase();
            const l = labelOf(v);
            return !q || `${l.name} ${l.hint}`.toLowerCase().includes(q);
          }}
        >
          <ComboboxTrigger
            id={id}
            aria-invalid={invalid}
            className="flex h-9 min-w-0 flex-1 items-center justify-between gap-1.5 rounded-2xl border border-transparent bg-input/50 px-3 text-sm whitespace-nowrap outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20"
          >
            <span
              className={`truncate ${value || none ? "" : "text-muted-foreground"}`}
            >
              {value ? labelOf(value).name : (none ?? "Choose")}
            </span>
          </ComboboxTrigger>
          {/* wide, so the summary beside each name has room; searchable past a few */}
          <ComboboxContent
            ref={popover.popupRef}
            aria-label={`Choose ${String(label).toLowerCase()}`}
            className="w-[28rem] min-w-[28rem] max-md:w-[calc(100vw-2rem)] max-md:min-w-0"
          >
            {shown.length > 6 && (
              <ComboboxInput
                showTrigger={false}
                aria-label={`Search ${String(label).toLowerCase()}`}
                placeholder={`Search ${items.length}`}
              />
            )}
            <ComboboxEmpty>Nothing by that name.</ComboboxEmpty>
            <ComboboxList>
              {(v: string) => {
                const l = labelOf(v);
                return (
                  <ComboboxItem
                    key={v || "∅"}
                    value={v}
                    disabled={shown.find((i) => i.value === v)?.disabled}
                  >
                    <span className="flex min-w-0 flex-1 items-baseline justify-between gap-4">
                      <span className="truncate">{l.name}</span>
                      {l.hint && (
                        <span className="max-w-48 shrink-0 truncate text-xs text-muted-foreground">
                          {l.hint}
                        </span>
                      )}
                    </span>
                  </ComboboxItem>
                );
              }}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
        {onCreate && (
          <Button
            variant="quiet"
            size="icon"
            aria-label={createLabel.replace(/…$/, "")}
            title={createLabel.replace(/…$/, "")}
            onClick={() =>
              void onCreate().then((created) => {
                if (created) onChange(created);
              })
            }
          >
            <Plus />
          </Button>
        )}
        {onOpen && value && known ? (
          <Button
            variant="quiet"
            size="icon"
            aria-label="Open"
            onClick={() => onOpen(value)}
          >
            <ExternalLink />
          </Button>
        ) : (
          openRoute &&
          value &&
          known && (
            <Button
              variant="quiet"
              size="icon"
              aria-label="Open"
              nativeButton={false}
              render={<a href={href(openRoute(value))} />}
            >
              <ExternalLink />
            </Button>
          )
        )}
      </span>
    </Row>
  );
}

/* several picks from a known list, shown as chips; typing filters the list */
export function ChipsRow({
  id,
  label,
  description,
  value,
  onChange,
  items,
  placeholder = "Add",
  empty = "No match.",
  error,
  disabled,
  createLabel,
  onCreate,
}: Common & {
  value: string[];
  onChange: (value: string[]) => void;
  items: Choice[];
  placeholder?: string;
  empty?: string;
  /* a last item that makes a new document and adds it to the chips */
  createLabel?: string;
  onCreate?: () => Promise<string | null>;
}) {
  const name = (v: string) =>
    items.find((i) => i.value === v)?.label ?? `${v} (unavailable)`;
  const all = items.map((i) => i.value);
  return (
    <Row label={label} description={description} htmlFor={id} error={error}>
      <span className="flex w-96 items-center gap-1 max-md:w-full">
        <Combobox
          multiple
          disabled={disabled}
          items={all}
          itemToStringLabel={name}
          value={value}
          filter={(v: string, query: string) =>
            name(v).toLowerCase().includes(query.trim().toLowerCase())
          }
          onValueChange={(v) => onChange(v ?? [])}
        >
          <ComboboxChips className="min-w-0 flex-1">
            <ComboboxValue>
              {(values: string[]) =>
                values.map((selected) => (
                  <ComboboxChip key={selected} showRemove={false}>
                    {name(selected)}
                    <Button
                      variant="ghost"
                      size="icon-xs"
                      aria-label={`Remove ${name(selected)}`}
                      className="-ml-0.5 size-4.5 opacity-50 hover:opacity-100"
                      onClick={() =>
                        onChange(value.filter((item) => item !== selected))
                      }
                    >
                      <XIcon />
                    </Button>
                  </ComboboxChip>
                ))
              }
            </ComboboxValue>
            <ComboboxChipsInput
              id={id}
              aria-invalid={error ? true : undefined}
              placeholder={value.length ? "" : placeholder}
            />
          </ComboboxChips>
          <ComboboxContent aria-label={`Choose ${String(label).toLowerCase()}`}>
            <ComboboxEmpty>{empty}</ComboboxEmpty>
            <ComboboxList>
              {(v: string) => (
                <ComboboxItem key={v} value={v}>
                  {name(v)}
                </ComboboxItem>
              )}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
        {onCreate && (
          <Button
            variant="quiet"
            size="icon"
            aria-label={(createLabel ?? "New").replace(/…$/, "")}
            title={(createLabel ?? "New").replace(/…$/, "")}
            onClick={() =>
              void onCreate().then((created) => {
                if (created) onChange([...value, created]);
              })
            }
          >
            <Plus />
          </Button>
        )}
      </span>
    </Row>
  );
}

export function TagsRow({
  id,
  label,
  description,
  value,
  onChange,
  placeholder = "Add a tag",
}: Common & {
  value: string[];
  onChange: (value: string[]) => void;
  placeholder?: string;
}) {
  const [text, setText] = useState("");
  const add = (raw: string) => {
    const next = Array.from(
      new Set(
        raw
          .split(/[\n,]/)
          .map((tag) => tag.trim())
          .filter(Boolean),
      ),
    ).filter((tag) => !value.includes(tag));
    if (next.length) onChange([...value, ...next]);
    setText("");
  };
  return (
    <Row label={label} description={description} htmlFor={id}>
      <div
        className="flex min-h-8 w-96 flex-wrap items-center gap-1 rounded-2xl border border-transparent bg-input/50 bg-clip-padding px-2.5 py-1 text-sm focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/30 max-md:w-full"
        onClick={(event) =>
          (
            event.currentTarget.querySelector("input") as HTMLInputElement | null
          )?.focus()
        }
      >
        {value.map((tag) => (
          <span
            key={tag}
            className="flex h-[calc(--spacing(5.25))] items-center gap-1 rounded-2xl bg-input px-1.5 pr-0.5 text-xs font-medium whitespace-nowrap dark:bg-input/60"
          >
            {tag}
            <Button
              variant="ghost"
              size="icon-xs"
              aria-label={`Remove ${tag}`}
              className="-ml-0.5 size-4.5 opacity-50 hover:opacity-100"
              onClick={() => onChange(value.filter((item) => item !== tag))}
            >
              <XIcon />
            </Button>
          </span>
        ))}
        <input
          id={id}
          value={text}
          placeholder={value.length ? "" : placeholder}
          onChange={(event) =>
            event.target.value.includes(",")
              ? add(event.target.value)
              : setText(event.target.value)
          }
          onBlur={() => text.trim() && add(text)}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              add(text);
            } else if (event.key === "Backspace" && !text && value.length) {
              onChange(value.slice(0, -1));
            }
          }}
          className="min-w-16 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
        />
      </div>
    </Row>
  );
}

export function SwitchRow({
  id,
  label,
  description,
  checked,
  onChange,
}: Common & { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Switch id={id} checked={checked} onCheckedChange={onChange} />
    </Row>
  );
}

export function FactRow({
  label,
  description,
  children,
  mono,
}: Omit<Common, "id"> & { children: ReactNode; mono?: boolean }) {
  return (
    <Row label={label} description={description}>
      <Fact mono={mono}>{children}</Fact>
    </Row>
  );
}

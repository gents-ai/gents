/* Shared configuration editor rows plus explicit Save/Cancel actions.
   Fields only update their local draft; persistence is user-controlled. */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { XIcon } from "lucide-react";
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
  ComboboxList,
  ComboboxValue,
} from "@gents/ui/components/combobox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Switch } from "@gents/ui/components/switch";
import { Textarea } from "@gents/ui/components/textarea";
import { Fact, Row, StackedRow } from "./rows";

export type Choice = { value: string; label: string };

type Common = { id: string; label: ReactNode; description?: ReactNode };

export function DraftActions({
  dirty,
  saving,
  error,
  onSave,
  onCancel,
}: {
  dirty: boolean;
  saving: boolean;
  error: string | null;
  onSave: () => void | Promise<void>;
  onCancel: () => void;
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
        <Button variant="brand" disabled={!dirty || saving} onClick={onSave}>
          {saving ? "Saving…" : "Save"}
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
    <Row label={label} description={description} htmlFor={id}>
      <Input
        id={id}
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
}: Common & {
  value: string;
  onChange: (v: string) => void;
  onCommit: () => void;
  placeholder?: string;
  rows?: number;
  mono?: boolean;
  stacked?: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [expanded, setExpanded] = useState(false);
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
    <Wrap label={label} description={description} htmlFor={id}>
      <Textarea
        ref={ref}
        id={id}
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
}: Common & {
  value: string;
  onChange: (v: string) => void;
  items: Choice[];
  none?: string;
}) {
  const all = none ? [{ value: "", label: none }, ...items] : items;
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Select items={all} value={value} onValueChange={(v) => onChange(v ?? "")}>
        <SelectTrigger id={id} className="w-72 max-md:w-full">
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

export function ChipsRow({
  id,
  label,
  description,
  value,
  onChange,
  items,
  placeholder = "Add",
  empty = "No match.",
}: Common & {
  value: string[];
  onChange: (value: string[]) => void;
  items: Choice[];
  placeholder?: string;
  empty?: string;
}) {
  const name = (value: string) =>
    items.find((item) => item.value === value)?.label ?? value;
  return (
    <Row label={label} description={description} htmlFor={id}>
      <Combobox
        multiple
        items={items.map((item) => item.value)}
        itemToStringLabel={name}
        value={value}
        onValueChange={(next) => onChange(next ?? [])}
      >
        <ComboboxChips className="w-96 max-md:w-full">
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
                    onClick={() => onChange(value.filter((item) => item !== selected))}
                  >
                    <XIcon />
                  </Button>
                </ComboboxChip>
              ))
            }
          </ComboboxValue>
          <ComboboxChipsInput id={id} placeholder={value.length ? "" : placeholder} />
        </ComboboxChips>
        <ComboboxContent>
          <ComboboxEmpty>{empty}</ComboboxEmpty>
          <ComboboxList>
            {(item: string) => (
              <ComboboxItem key={item} value={item}>
                {name(item)}
              </ComboboxItem>
            )}
          </ComboboxList>
        </ComboboxContent>
      </Combobox>
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

import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import {
  ChevronRight,
  ExternalLink,
  FolderOpen,
  Package,
  Pencil,
  Plus,
  XIcon,
} from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "@gents/ui/components/toggle-group";
import { Markdown } from "../Markdown";
import { canPickDirectory, pickDirectory } from "../../../lib/nativeShell";
import { Input } from "@gents/ui/components/input";
import { Button } from "@gents/ui/components/button";
import {
  Combobox,
  ComboboxChip,
  ComboboxChips,
  ComboboxChipsInput,
  ComboboxContent,
  ComboboxCollection,
  ComboboxEmpty,
  ComboboxGroup,
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
import { focusFirstProblem } from "./draft";
import { usePackGroups } from "./ListDetail";

/** `pack` names the pack a document came from; a reference list groups
    those under it, after the person's own */
export type Choice = { value: string; label: string; pack?: string | null };

type Common = {
  id: string;
  label: ReactNode;
  description?: ReactNode;
  error?: string;
  disabled?: boolean;
};

type DraftLike = {
  dirty: boolean;
  saving: boolean;
  error: string | null;
  problems?: Partial<Record<string, string>>;
  save: () => unknown;
  reset: () => void;
};

/** Save and Cancel for a draft. While there is something to save they stay in
    view at the foot of the scroller, and rest in place, without a background,
    once the form's end is reached. */
export function DraftActions({
  draft,
  fields,
  problems = draft.problems ?? {},
  dirty = draft.dirty,
  error = draft.error,
  onSave = () => draft.save(),
  onCancel = draft.reset,
  saveLabel = "Save",
}: {
  draft: DraftLike;
  fields?: Partial<Record<string, string>>;
  problems?: Partial<Record<string, string>>;
  dirty?: boolean;
  error?: string | null;
  onSave?: () => unknown;
  onCancel?: () => void;
  saveLabel?: string;
}) {
  const { saving } = draft;
  const sticky = dirty || saving || !!error;
  const barRef = useRef<HTMLDivElement>(null);
  const [floating, setFloating] = useState(false);
  /* arriving: it just became sticky and may slide up to the scroller's foot;
     leaving: it was floating when the edits went, and slides back down first */
  const [motion, setMotion] = useState<"arriving" | "entering" | "leaving" | null>(
    null,
  );
  const [wasSticky, setWasSticky] = useState(sticky);
  if (wasSticky !== sticky) {
    setWasSticky(sticky);
    setMotion(sticky ? "arriving" : floating && !reducedMotion() ? "leaving" : null);
  }
  const pinned = sticky || motion === "leaving";
  useLayoutEffect(() => {
    const bar = barRef.current;
    if (motion !== "arriving" || !bar) return;
    const stuck = bar.getBoundingClientRect().top;
    bar.style.position = "static";
    const resting = bar.getBoundingClientRect().top;
    bar.style.position = "";
    const lifted = resting - stuck > 1;
    setFloating(lifted);
    setMotion(lifted && !reducedMotion() ? "entering" : null);
  }, [motion]);
  useEffect(() => {
    const bar = barRef.current;
    if (!bar || !pinned || typeof IntersectionObserver === "undefined") {
      setFloating(false);
      return;
    }
    /* stuck one pixel past the scroller's foot, so a floating bar is never
       wholly in view and a resting one is */
    const observer = new IntersectionObserver(
      ([entry]) => setFloating(entry!.intersectionRatio < 1),
      { threshold: 1 },
    );
    observer.observe(bar);
    return () => observer.disconnect();
  }, [pinned]);
  return (
    <div
      ref={barRef}
      data-testid="draft-actions"
      data-floating={floating || undefined}
      onAnimationEnd={() => setMotion(null)}
      className={`${pinned ? "sticky -bottom-px z-10" : ""} pb-4 ${motion === "entering" ? "animate-in fade-in-0 slide-in-from-bottom-full duration-200 ease-out" : motion === "leaving" ? "animate-out fade-out-0 slide-out-to-bottom-full duration-150 ease-in fill-mode-forwards" : ""}`}
    >
      <div
        className={`flex flex-wrap items-center justify-end gap-x-3 gap-y-1 rounded-[1.25rem] border py-1.5 pr-1.5 pl-4 transition-[background-color,border-color,box-shadow] ${floating ? "border-border bg-raised/95 shadow-2xl shadow-black/60 backdrop-blur" : "border-transparent"}`}
      >
        {error ? (
          <p role="alert" className="mr-auto min-w-0 text-xs text-destructive">
            {error}
          </p>
        ) : (
          dirty && (
            <span className="mr-auto text-xs text-muted-foreground">
              Unsaved changes
            </span>
          )
        )}
        <div className="flex gap-1.5">
          <Button
            size="sm"
            variant="quiet"
            disabled={!dirty || saving}
            onClick={onCancel}
          >
            Cancel
          </Button>
          <Button
            size="sm"
            variant="brand"
            disabled={!dirty || saving}
            onClick={() => {
              if (!focusFirstProblem(problems, fields)) void onSave();
            }}
          >
            {saving ? "Saving…" : saveLabel}
          </Button>
        </div>
      </div>
    </div>
  );
}

const reducedMotion = () =>
  typeof matchMedia !== "undefined" &&
  matchMedia("(prefers-reduced-motion: reduce)").matches;

const blurOnEnter = (e: React.KeyboardEvent<HTMLElement>) => {
  if (e.key === "Enter" && !e.shiftKey) (e.target as HTMLElement).blur();
};

export function TextRow({
  id,
  label,
  description,
  value,
  onChange,
  placeholder,
  mono,
  password,
  wide,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
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
        onKeyDown={blurOnEnter}
        placeholder={placeholder}
        className={`${wide ? "w-96 max-md:w-full" : "w-72 max-md:w-full"} ${mono ? "font-mono text-xs" : ""}`}
      />
    </Row>
  );
}

export function NumberRow({
  id,
  label,
  description,
  value,
  onChange,
  placeholder,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
}) {
  return (
    <Row label={label} description={description} htmlFor={id} error={error}>
      <Input
        id={id}
        aria-invalid={error ? true : undefined}
        disabled={disabled}
        inputMode="decimal"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={blurOnEnter}
        placeholder={placeholder}
        className="w-36"
      />
    </Row>
  );
}

function useOverflowing(
  ref: RefObject<HTMLElement | null>,
  watching: boolean,
  content: unknown,
  expanded: boolean,
) {
  const [overflowing, setOverflowing] = useState(false);
  useEffect(() => {
    const element = ref.current;
    if (!watching || !element || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() =>
      setOverflowing(element.scrollHeight > element.clientHeight + 1),
    );
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref, watching, content, expanded]);
  return overflowing;
}

export function AreaRow({
  id,
  label,
  description,
  value,
  onChange,
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
  placeholder?: string;
  rows?: number;
  mono?: boolean;
  stacked?: boolean;
  expandedByDefault?: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [expanded, setExpanded] = useState(expandedByDefault);
  const overflowing = useOverflowing(ref, stacked === true, value, expanded);
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
        placeholder={placeholder}
        rows={rows}
        style={clipped ? { maxHeight: `calc(${rows}lh + 1rem + 2px)` } : undefined}
        /* the kit's textarea grows with its text; beside a label it stops
           at a height and scrolls, as a stacked one does behind Show all */
        className={`${stacked ? "w-full" : "max-h-48 w-96 max-md:w-full"} ${mono ? "font-mono text-xs" : ""}`}
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

/** A stacked field of Markdown, read rendered at a set height, or raw, as
    the text it is edited in. */
export function MarkdownRow({
  id,
  label,
  description,
  value,
  onChange,
  empty,
  rows = 6,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
  /** what the rendered view says while there is no text */
  empty: string;
  rows?: number;
}) {
  const field = useRef<HTMLTextAreaElement>(null);
  const read = useRef<HTMLDivElement>(null);
  const [view, setView] = useState<"rendered" | "raw">("rendered");
  const [expanded, setExpanded] = useState(false);
  const raw = view === "raw";
  const overflowing = useOverflowing(
    raw ? field : read,
    true,
    `${view}${value}`,
    expanded,
  );
  const height = expanded ? undefined : { maxHeight: `calc(${rows}lh + 1rem + 2px)` };
  return (
    <StackedRow
      label={label}
      description={description}
      htmlFor={raw ? id : undefined}
      error={error}
      action={
        <ToggleGroup
          aria-label={`${String(label)} view`}
          value={[view]}
          onValueChange={(next) => {
            const chosen = next[0] as "rendered" | "raw" | undefined;
            if (chosen) setView(chosen);
          }}
          className="gap-0.5 rounded-xl bg-muted/60 p-0.5"
        >
          {(
            [
              ["rendered", "Rendered"],
              ["raw", "Raw"],
            ] as const
          ).map(([v, name]) => (
            <ToggleGroupItem
              key={v}
              value={v}
              className="h-6 rounded-lg px-2 text-xs text-muted-foreground hover:bg-transparent hover:text-foreground aria-pressed:bg-background aria-pressed:text-foreground aria-pressed:shadow-sm"
            >
              {name}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      }
    >
      {raw ? (
        <Textarea
          ref={field}
          id={id}
          aria-invalid={error ? true : undefined}
          readOnly={disabled}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          rows={rows}
          style={height}
          className="w-full"
        />
      ) : (
        <div
          ref={read}
          style={height}
          className="prose-app min-w-0 overflow-hidden rounded-xl bg-input/50 px-3 py-2 text-sm break-words"
        >
          {value.trim() ? (
            <Markdown>{value}</Markdown>
          ) : (
            <p className="text-muted-foreground">{empty}</p>
          )}
        </div>
      )}
      {(overflowing || expanded) && (
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
    </StackedRow>
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

export function PathRow({
  id,
  label,
  description,
  value,
  onChange,
  placeholder,
  error,
  disabled,
}: Common & {
  value: string;
  onChange: (v: string) => void;
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
      if (picked) onChange(picked);
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
          onKeyDown={blurOnEnter}
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

/* a pack's heading in a reference list: a toggle, unless a search or the
   chosen document holds it open */
function PackHeading({
  pack,
  count,
  open,
  onToggle,
}: {
  pack: string;
  count: number;
  open: boolean;
  onToggle?: () => void;
}) {
  const inner = (
    <>
      <ChevronRight
        aria-hidden
        className={`size-3.5 shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
      />
      <Package aria-hidden className="size-3.5 shrink-0" />
      <span className="truncate">{pack}</span>
      <span className="shrink-0">· {count}</span>
    </>
  );
  return (
    /* not the group's ComboboxLabel: Base UI hides that from assistive
       technology, which would hide the toggle too; the group is named by
       aria-label instead */
    <div className="text-xs">
      {onToggle ? (
        <button
          type="button"
          aria-expanded={open}
          /* the search field keeps focus, so typing goes on working */
          onMouseDown={(event) => event.preventDefault()}
          onClick={onToggle}
          className="flex w-full cursor-pointer items-center gap-1.5 rounded-md px-2 py-1.5 text-left text-xs text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          {inner}
        </button>
      ) : (
        <span className="flex items-center gap-1.5 px-2 py-1.5 text-xs text-muted-foreground">
          {inner}
        </span>
      )}
    </div>
  );
}

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
  onOpen?: (id: string) => void;
}) {
  const all = none ? [{ value: "", label: none }, ...items] : items;
  const known = all.some((i) => i.value === value);
  const shown = known ? all : [...all, { value, label: `${value} (unavailable)` }];
  const invalid = error ? true : !known && value ? true : undefined;
  const popover = useExclusivePopover();
  const labelOf = (v: string) => {
    const full = shown.find((i) => i.value === v)?.label ?? v;
    const at = full.indexOf(" · ");
    return at === -1
      ? { name: full, hint: "" }
      : { name: full.slice(0, at), hint: full.slice(at + 3) };
  };
  const packOf = (v: string) => shown.find((i) => i.value === v)?.pack ?? "";
  const packs = [
    ...new Set(shown.map((i) => i.pack).filter(Boolean)),
  ].sort() as string[];
  const [query, setQuery] = useState("");
  const packGroups = usePackGroups(`ref:${String(label)}`);
  const searching = query.trim() !== "";
  /* a pack's documents fold under its name, as in the agent picker; a
     search or the chosen document holds its pack open */
  const held = (pack: string) => searching || packOf(value) === pack;
  const isOpen = (pack: string) => held(pack) || packGroups.isOpen(pack, false);
  /* the person's own first, unlabelled, then each pack's under its name.
     A folded pack keeps its items here, unrendered: Base UI drops a group
     with none, and its heading with it */
  const groups = packs.length
    ? [
        { value: "", items: shown.filter((i) => !i.pack).map((i) => i.value) },
        ...packs.map((pack) => ({
          value: pack,
          items: shown.filter((i) => i.pack === pack).map((i) => i.value),
        })),
      ].filter((g) => g.items.length)
    : null;
  const option = (v: string) => {
    const l = labelOf(v);
    return (
      <ComboboxItem key={v || "∅"} value={v}>
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
          items={groups ?? shown.map((i) => i.value)}
          value={value}
          onValueChange={(v) => {
            popover.onOpenChange(false);
            onChange(v ?? "");
          }}
          open={popover.open}
          onOpenChange={(next) => {
            if (!next) setQuery("");
            popover.onOpenChange(next);
          }}
          onInputValueChange={(next) => setQuery(String(next))}
          onOpenChangeComplete={popover.onOpenChangeComplete}
          disabled={disabled}
          itemToStringLabel={(v: string) => labelOf(v).name}
          filter={(v: string, query: string) => {
            const q = query.trim().toLowerCase();
            const l = labelOf(v);
            return !q || `${l.name} ${l.hint} ${packOf(v)}`.toLowerCase().includes(q);
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
              {groups
                ? (group: { value: string; items: string[] }) => (
                    <ComboboxGroup
                      key={group.value || "own"}
                      items={group.items}
                      aria-label={group.value || undefined}
                    >
                      {group.value && (
                        <PackHeading
                          pack={group.value}
                          count={shown.filter((i) => i.pack === group.value).length}
                          open={isOpen(group.value)}
                          onToggle={
                            held(group.value)
                              ? undefined
                              : () => packGroups.toggle(group.value, false)
                          }
                        />
                      )}
                      {(!group.value || isOpen(group.value)) && (
                        <ComboboxCollection>{option}</ComboboxCollection>
                      )}
                    </ComboboxGroup>
                  )
                : option}
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
  createLabel?: string;
  onCreate?: () => Promise<string | null>;
}) {
  const name = (v: string) =>
    items.find((i) => i.value === v)?.label ?? `${v} (unavailable)`;
  const all = items.map((i) => i.value);
  const packOf = (v: string) => items.find((i) => i.value === v)?.pack ?? "";
  const packs = [
    ...new Set(items.map((i) => i.pack).filter(Boolean)),
  ].sort() as string[];
  const [query, setQuery] = useState("");
  const packGroups = usePackGroups(`chips:${String(label)}`);
  /* folded as in a reference field; a search, or a pick from the pack,
     holds it open */
  const held = (pack: string) =>
    query.trim() !== "" || value.some((v) => packOf(v) === pack);
  const isOpen = (pack: string) => held(pack) || packGroups.isOpen(pack, false);
  const groups = packs.length
    ? [
        { value: "", items: items.filter((i) => !i.pack).map((i) => i.value) },
        ...packs.map((pack) => ({
          value: pack,
          items: items.filter((i) => i.pack === pack).map((i) => i.value),
        })),
      ].filter((g) => g.items.length)
    : null;
  const option = (v: string) => (
    <ComboboxItem key={v} value={v}>
      {name(v)}
    </ComboboxItem>
  );
  return (
    <Row label={label} description={description} htmlFor={id} error={error}>
      <span className="flex w-96 items-center gap-1 max-md:w-full">
        <Combobox
          multiple
          disabled={disabled}
          items={groups ?? all}
          itemToStringLabel={name}
          value={value}
          filter={(v: string, query: string) =>
            `${name(v)} ${packOf(v)}`.toLowerCase().includes(query.trim().toLowerCase())
          }
          onInputValueChange={(next) => setQuery(String(next))}
          onOpenChange={(next) => {
            if (!next) setQuery("");
          }}
          onValueChange={(v) => onChange(v ?? [])}
        >
          <ComboboxChips className="max-h-24 min-w-0 flex-1 overflow-y-auto">
            <ComboboxValue>
              {(values: string[]) =>
                values.map((selected) => (
                  <ComboboxChip key={selected} showRemove={false} className="max-w-48">
                    <span className="min-w-0 truncate" title={name(selected)}>
                      {name(selected)}
                    </span>
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
              {groups
                ? (group: { value: string; items: string[] }) => (
                    <ComboboxGroup
                      key={group.value || "own"}
                      items={group.items}
                      aria-label={group.value || undefined}
                    >
                      {group.value && (
                        <PackHeading
                          pack={group.value}
                          count={group.items.length}
                          open={isOpen(group.value)}
                          onToggle={
                            held(group.value)
                              ? undefined
                              : () => packGroups.toggle(group.value, false)
                          }
                        />
                      )}
                      {(!group.value || isOpen(group.value)) && (
                        <ComboboxCollection>{option}</ComboboxCollection>
                      )}
                    </ComboboxGroup>
                  )
                : option}
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
        className="flex max-h-24 min-h-8 w-96 flex-wrap items-center gap-1 overflow-y-auto rounded-2xl border border-transparent bg-input/50 bg-clip-padding px-2.5 py-1 text-sm focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/30 max-md:w-full"
        onClick={(event) =>
          (
            event.currentTarget.querySelector("input") as HTMLInputElement | null
          )?.focus()
        }
      >
        {value.map((tag) => (
          <span
            key={tag}
            className="flex h-[calc(--spacing(5.25))] max-w-48 items-center gap-1 rounded-2xl bg-input px-1.5 pr-0.5 text-xs font-medium whitespace-nowrap dark:bg-input/60"
          >
            <span className="min-w-0 truncate" title={tag}>
              {tag}
            </span>
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

export function LongFactRow({
  label,
  description,
  children,
}: Omit<Common, "id"> & { children: string }) {
  const ref = useRef<HTMLParagraphElement>(null);
  const [expanded, setExpanded] = useState(false);
  const overflowing = useOverflowing(ref, true, children, expanded);
  return (
    <StackedRow label={label} description={description}>
      <p
        ref={ref}
        className={`text-sm wrap-break-word whitespace-pre-wrap text-muted-foreground ${expanded ? "" : "line-clamp-3"}`}
      >
        {children}
      </p>
      {(overflowing || expanded) && (
        <Button
          variant="quiet"
          size="sm"
          className="-mt-1 -ml-3 self-start"
          onClick={() => setExpanded((open) => !open)}
        >
          {expanded ? "Show less" : "Show all"}
        </Button>
      )}
    </StackedRow>
  );
}

import type { NodeView } from "../../../hooks/fleetStore";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import {
  Combobox,
  ComboboxCollection,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxGroup,
  ComboboxInput,
  ComboboxItem,
  ComboboxLabel,
  ComboboxList,
  ComboboxTrigger,
} from "@gents/ui/components/combobox";
import { SEARCH_FIELD } from "@/lib/textAssist";

/** Typing a name instead of choosing one. */
export const OTHER = "\u0000other";

/* Core collections a triggered task doesn't write to itself, so watching
   them can't set off its own trigger. Requests, messages and tool calls are
   left out for that reason. */
const CORE = ["MailboxItem"];

type Group = { value: string; items: string[] };

/** A collection's name in words: MailboxItem reads "Mailbox items". */
export function collectionInWords(name: string): string {
  const words = name
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/[_-]+/g, " ")
    .trim()
    .toLowerCase();
  const plural = words.endsWith("s") ? words : `${words}s`;
  return plural.charAt(0).toUpperCase() + plural.slice(1);
}

const listOf = <T,>(value: readonly T[] | null | undefined): readonly T[] =>
  Array.isArray(value) ? value : [];

/** A surface's tool declarations. The generated type says a list, but the
    runtime stores and sends them wrapped as `{ entries: [...] }`. */
function surfaceEntries(raw: unknown): readonly { collection?: unknown }[] {
  if (Array.isArray(raw)) return raw;
  const wrapped = (raw as { entries?: unknown } | null)?.entries;
  return Array.isArray(wrapped) ? wrapped : [];
}

/** What the node can be watched for, from what its snapshot knows: the
    core collections that are safe to watch, the collections its packs'
    tools read and write, and those its event triggers already watch. */
export function collectionGroups(node: NodeView): Group[] {
  const fromPacks = new Set<string>();
  for (const surface of listOf(node.datastoreToolSurfaces))
    for (const entry of surfaceEntries(surface.entries))
      if (typeof entry?.collection === "string") fromPacks.add(entry.collection);
  for (const tools of listOf(node.tools)) {
    for (const c of listOf(tools.datastore?.defra_query_collections)) fromPacks.add(c);
    for (const c of listOf(tools.datastore?.write_collections)) fromPacks.add(c);
    for (const c of listOf(tools.built_ins?.p2p_collections)) fromPacks.add(c);
  }
  for (const c of CORE) fromPacks.delete(c);
  const watched = new Set(listOf(node.eventSources).map((e) => e.source_collection));
  for (const c of [...CORE, ...fromPacks]) watched.delete(c);
  return [
    { value: "Gents", items: CORE },
    { value: "From your packs", items: [...fromPacks].sort() },
    { value: "Already watched", items: [...watched].sort() },
    { value: "", items: [OTHER] },
  ].filter((g) => g.items.length > 0);
}

const TRIGGER =
  "flex h-9 w-full items-center justify-between gap-1.5 rounded-2xl border border-transparent bg-input/50 px-3 text-sm whitespace-nowrap outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20";

/** Which collection an event trigger watches, chosen by its name in words,
    or typed when it isn't listed. */
export function CollectionPicker({
  id,
  node,
  value,
  onPick,
}: {
  id: string;
  node: NodeView;
  /** a collection's name, OTHER while one is being typed, or "" */
  value: string;
  onPick: (value: string) => void;
}) {
  const popover = useExclusivePopover();
  const groups = collectionGroups(node);
  const label = (v: string) => (v === OTHER ? "Other…" : collectionInWords(v));
  return (
    <Combobox
      items={groups}
      value={value || null}
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
      onValueChange={(v) => {
        if (v == null) return;
        popover.onOpenChange(false);
        onPick(v as string);
      }}
      itemToStringLabel={label}
      filter={(v: string, query: string) => {
        const q = query.trim().toLowerCase();
        return !q || v === OTHER || `${label(v)} ${v}`.toLowerCase().includes(q);
      }}
    >
      <ComboboxTrigger id={id} className={TRIGGER}>
        <span className={`truncate ${value ? "" : "text-muted-foreground"}`}>
          {value ? label(value) : "Choose what to watch"}
        </span>
      </ComboboxTrigger>
      <ComboboxContent
        ref={popover.popupRef}
        aria-label="Choose what to watch"
        className="w-80 min-w-80 max-md:w-[calc(100vw-2rem)] max-md:min-w-0"
      >
        <ComboboxInput
          showTrigger={false}
          aria-label="Search collections"
          {...SEARCH_FIELD}
        />
        <ComboboxEmpty>Nothing by that name.</ComboboxEmpty>
        <ComboboxList>
          {(group: Group) => (
            <ComboboxGroup
              key={group.value || "other"}
              items={group.items}
              aria-label={group.value || undefined}
            >
              {group.value && <ComboboxLabel>{group.value}</ComboboxLabel>}
              <ComboboxCollection>
                {(v: string) => (
                  <ComboboxItem key={v} value={v}>
                    <span className="flex min-w-0 flex-1 items-baseline justify-between gap-4">
                      <span className="truncate">{label(v)}</span>
                      {v !== OTHER && (
                        <span className="max-w-36 shrink-0 truncate font-mono text-xs text-muted-foreground">
                          {v}
                        </span>
                      )}
                    </span>
                  </ComboboxItem>
                )}
              </ComboboxCollection>
            </ComboboxGroup>
          )}
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  );
}

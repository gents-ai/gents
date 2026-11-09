import type { NodeView } from "../../../hooks/fleetStore";
import { useState } from "react";
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
} from "@gents/ui/components/combobox";

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
  }
  for (const c of CORE) fromPacks.delete(c);
  const watched = new Set(listOf(node.eventSources).map((e) => e.source_collection));
  for (const c of [...CORE, ...fromPacks]) watched.delete(c);
  return [
    { value: "Gents", items: CORE },
    { value: "From your packs", items: [...fromPacks].sort() },
    { value: "Already watched", items: [...watched].sort() },
  ].filter((g) => g.items.length > 0);
}

/* a name the runtime would accept as a collection's */
const COLLECTION_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Which collection an event trigger watches. The field is the search:
    what it lists is named in words, and a name it doesn't list can be
    typed and watched as it is. */
export function CollectionPicker({
  id,
  node,
  value,
  onPick,
}: {
  id: string;
  node: NodeView;
  /** a collection's name, or "" */
  value: string;
  onPick: (value: string) => void;
}) {
  const [query, setQuery] = useState("");
  const listed = collectionGroups(node);
  const known = new Set(listed.flatMap((g) => g.items));
  const typed = query.trim();
  const own = [
    ...new Set(
      [value, COLLECTION_NAME.test(typed) ? typed : ""].filter(
        (v) => v && !known.has(v),
      ),
    ),
  ];
  const groups = own.length ? [...listed, { value: "", items: own }] : listed;
  const label = (v: string) => (known.has(v) ? collectionInWords(v) : v);
  const fromPacks = listed.some((g) => g.value === "From your packs");
  return (
    <Combobox
      items={groups}
      value={value || null}
      onValueChange={(v) => v != null && onPick(v as string)}
      onInputValueChange={(v) => setQuery(String(v))}
      itemToStringLabel={label}
      filter={(v: string, q: string) => {
        const t = q.trim().toLowerCase();
        return !t || !known.has(v) || `${label(v)} ${v}`.toLowerCase().includes(t);
      }}
    >
      <ComboboxInput id={id} placeholder="Choose, or type a collection's name" />
      <ComboboxContent aria-label="Collections to watch">
        <ComboboxEmpty>Type a collection's name to watch it.</ComboboxEmpty>
        <ComboboxList>
          {(group: Group) => (
            <ComboboxGroup
              key={group.value || "typed"}
              items={group.items}
              aria-label={group.value || undefined}
            >
              {group.value && <ComboboxLabel>{group.value}</ComboboxLabel>}
              <ComboboxCollection>
                {(v: string) => (
                  <ComboboxItem key={v} value={v}>
                    {known.has(v) ? (
                      <span className="flex min-w-0 flex-1 items-baseline justify-between gap-4">
                        <span className="truncate">{label(v)}</span>
                        <span className="max-w-40 shrink-0 truncate font-mono text-xs text-muted-foreground">
                          {v}
                        </span>
                      </span>
                    ) : (
                      <span className="truncate">
                        Watch <span className="font-mono">{v}</span>
                      </span>
                    )}
                  </ComboboxItem>
                )}
              </ComboboxCollection>
            </ComboboxGroup>
          )}
        </ComboboxList>
        {!fromPacks && (
          <p className="px-3 pt-1 pb-2.5 text-xs text-muted-foreground">
            Packs you install can add more.
          </p>
        )}
      </ComboboxContent>
    </Combobox>
  );
}

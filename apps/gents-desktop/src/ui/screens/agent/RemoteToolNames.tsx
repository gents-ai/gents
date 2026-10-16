import { useState } from "react";
import type { ToolServiceToolView } from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Checkbox } from "@gents/ui/components/checkbox";
import { Input } from "@gents/ui/components/input";
import { Textarea } from "@gents/ui/components/textarea";
import { StackedRow } from "./rows";
import { errorText } from "../../../lib/errorText";

const SEARCH_FROM = 8;

/* the background picker keys its rows by name */
const namesIn = (text: string) => [
  ...new Set(
    text
      .split("\n")
      .map((name) => name.trim())
      .filter(Boolean),
  ),
];

export function RemoteToolNames({
  id,
  label,
  selected,
  onChange,
  discover,
}: {
  id: string;
  label: string;
  selected: string[];
  onChange: (values: string[]) => void;
  discover: () => Promise<ToolServiceToolView[]>;
}) {
  /* the text as typed: a line being started is not a name yet, and
     re-joining the names on each keystroke would drop it */
  const [text, setText] = useState(selected.join("\n"));
  const shown =
    namesIn(text).join("\n") === selected.join("\n") ? text : selected.join("\n");
  const [tools, setTools] = useState<ToolServiceToolView[] | null>(null);
  const [search, setSearch] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tick = (name: string, checked: boolean) => {
    const next = checked
      ? [...selected, name]
      : selected.filter((selectedName) => selectedName !== name);
    setText(next.join("\n"));
    onChange(next);
  };
  const searching = (tools?.length ?? 0) >= SEARCH_FROM;
  const visible = (tools ?? []).filter(
    (tool) =>
      !searching ||
      `${tool.name} ${tool.description ?? ""}`
        .toLowerCase()
        .includes(search.toLowerCase()),
  );
  return (
    <StackedRow
      label={label}
      htmlFor={id}
      description="Exact tool names, one per line, or ticked from what the service reports. Empty grants no tools; discovery never grants new tools automatically."
    >
      <Textarea
        id={id}
        rows={3}
        className="w-full font-mono text-xs"
        value={shown}
        onChange={(event) => {
          setText(event.target.value);
          onChange(namesIn(event.target.value));
        }}
      />
      <div className="flex flex-wrap items-center gap-3">
        <Button
          size="sm"
          variant="outline"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              setSearch("");
              setTools(await discover());
            } catch (error) {
              setError(errorText(error));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Discovering…" : tools ? "Discover again" : "Discover remote tools"}
        </Button>
        {tools && (
          <span className="text-sm text-muted-foreground">
            {tools.length === 1 ? "1 tool reported" : `${tools.length} tools reported`}
          </span>
        )}
      </div>
      {error && (
        <p role="alert" className="text-sm wrap-break-word text-destructive">
          {error}
        </p>
      )}
      {tools && tools.length > 0 && (
        <div className="space-y-2">
          {searching && (
            <Input
              aria-label="Search reported tools"
              placeholder="Search by name"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
          )}
          <ul
            aria-label="Reported tools"
            className="max-h-64 space-y-1 overflow-y-auto rounded-xl border border-border/60 p-1"
          >
            {visible.map((tool) => (
              <li key={tool.name}>
                <label className="flex cursor-pointer items-start gap-3 rounded-lg px-2 py-1.5 hover:bg-accent">
                  <Checkbox
                    className="mt-0.5"
                    checked={selected.includes(tool.name)}
                    onCheckedChange={(checked) => tick(tool.name, checked === true)}
                  />
                  <span className="min-w-0">
                    <span className="block font-mono text-xs break-all">
                      {tool.name}
                    </span>
                    {tool.description && (
                      <span className="block text-xs text-muted-foreground">
                        {tool.description}
                      </span>
                    )}
                  </span>
                </label>
              </li>
            ))}
          </ul>
        </div>
      )}
    </StackedRow>
  );
}

import { useState } from "react";
import { Input } from "@gents/ui/components/input";

/** Select explicit document references; discovery never implicitly grants access. */
export function DocumentSelection({
  label,
  options,
  selected,
  onChange,
}: {
  label: string;
  options: { value: string; label: string; description?: string | null }[];
  selected: string[];
  onChange: (values: string[]) => void;
}) {
  const [search, setSearch] = useState("");
  const known = new Set(options.map((option) => option.value));
  const all = [
    ...options,
    ...selected
      .filter((id) => !known.has(id))
      .map((id) => ({
        value: id,
        label: `${id} (unavailable)`,
        description: "Remove this missing reference before saving.",
      })),
  ];
  const visible = all.filter((option) =>
    `${option.label} ${option.value} ${option.description ?? ""}`
      .toLowerCase()
      .includes(search.toLowerCase()),
  );
  return (
    <fieldset className="space-y-3 p-4">
      <legend className="px-1 text-sm font-medium">{label}</legend>
      <Input
        aria-label={`Search ${label.toLowerCase()}`}
        placeholder="Search by name"
        value={search}
        onChange={(event) => setSearch(event.target.value)}
      />
      <div className="max-h-64 space-y-2 overflow-auto">
        {visible.map((option) => (
          <label
            key={option.value}
            className="flex cursor-pointer items-start gap-3 rounded-lg p-2 hover:bg-muted"
          >
            <input
              type="checkbox"
              className="mt-1"
              checked={selected.includes(option.value)}
              onChange={(event) =>
                onChange(
                  event.target.checked
                    ? [...selected, option.value]
                    : selected.filter((id) => id !== option.value),
                )
              }
            />
            <span className="min-w-0 break-words text-sm">
              {option.label}
              {option.description && (
                <span className="block text-xs text-muted-foreground">
                  {option.description}
                </span>
              )}
            </span>
          </label>
        ))}
        {!visible.length && (
          <p className="text-sm text-muted-foreground">
            {all.length ? "No matching documents." : "No documents available."}
          </p>
        )}
      </div>
    </fieldset>
  );
}

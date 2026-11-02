import { useState } from "react";
import { RotateCcw, Search } from "lucide-react";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { FieldRow } from "@gents/ui/components/field";
import { Input } from "@gents/ui/components/input";
import { Switch } from "@gents/ui/components/switch";
import { FLAGS } from "@/lib/flags";
import { preferences, usePreferences, useRelaunchPending } from "@/preferences";
import { Group } from "../agent/rows";
import { Hint } from "../Hint";

export function ExperimentalPanel() {
  const settings = usePreferences((p) => p.flags);
  const relaunchPending = useRelaunchPending();
  const [search, setSearch] = useState("");
  const words = search.trim().toLowerCase();
  const shown = FLAGS.filter((f) =>
    `${f.name} ${f.description} ${f.id}`.toLowerCase().includes(words),
  );
  return (
    <div>
      <header className="mb-6 flex items-start justify-between gap-4">
        <div>
          <h2 className="font-heading text-lg text-heading">Experimental features</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Try features that aren’t finished. They may change or go away.
          </p>
        </div>
        <Hint label="Put every feature back to its default">
          <Button
            variant="outline"
            size="sm"
            disabled={Object.keys(settings).length === 0}
            onClick={preferences.resetFlags}
          >
            Reset all
          </Button>
        </Hint>
      </header>
      <Group>
        <div className="flex items-center gap-2 px-5 py-2.5">
          <Search className="size-4 shrink-0 text-muted-foreground" />
          <Input
            type="search"
            aria-label="Search experimental features"
            placeholder="Search by name or description"
            className="h-8 border-0 bg-transparent px-0 shadow-none focus-visible:ring-0 dark:bg-transparent"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            onKeyDown={(event) => {
              if (event.key !== "Escape" || !search) return;
              event.preventDefault();
              event.stopPropagation();
              setSearch("");
            }}
          />
        </div>
        {shown.length === 0 && (
          <p className="px-5 py-3 text-sm text-muted-foreground">
            No experimental features match
          </p>
        )}
        {shown.map((f) => {
          const setting = settings[f.id];
          const on = setting === undefined ? f.default : setting === "on";
          return (
            <FieldRow key={f.id} className="flex items-center gap-4 py-2.5">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2 text-sm font-medium">
                  {f.name}
                  {f.restart && (
                    <Hint label="Takes effect after relaunch">
                      <Badge variant="outline" className="font-normal">
                        Relaunch
                      </Badge>
                    </Hint>
                  )}
                  {setting !== undefined && (
                    <span className="text-xs font-normal text-muted-foreground">
                      Changed
                    </span>
                  )}
                </div>
                <p
                  className="truncate text-xs text-muted-foreground"
                  title={f.description}
                >
                  {f.description}
                </p>
              </div>
              {setting !== undefined && (
                <Hint label={`Reset to default (${f.default ? "on" : "off"})`}>
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`Reset ${f.name} to default`}
                    onClick={() => preferences.setFlag(f.id, "default")}
                  >
                    <RotateCcw />
                  </Button>
                </Hint>
              )}
              <Switch
                aria-label={f.name}
                checked={on}
                onCheckedChange={(checked) =>
                  preferences.setFlag(f.id, checked ? "on" : "off")
                }
              />
            </FieldRow>
          );
        })}
      </Group>
      {relaunchPending && (
        <div
          data-testid="relaunch-bar"
          className="sticky -bottom-px z-10 pb-4 motion-safe:animate-in motion-safe:fade-in-0 motion-safe:slide-in-from-bottom-full motion-safe:duration-200 motion-safe:ease-out"
        >
          <div className="flex flex-wrap items-center justify-end gap-x-3 gap-y-1 rounded-[1.25rem] border border-border bg-raised/95 py-1.5 pr-1.5 pl-4 shadow-2xl shadow-black/60 backdrop-blur">
            <span className="mr-auto text-xs text-muted-foreground">
              Some changes apply after the app relaunches
            </span>
            <Button size="sm" variant="brand" onClick={() => window.location.reload()}>
              Relaunch
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

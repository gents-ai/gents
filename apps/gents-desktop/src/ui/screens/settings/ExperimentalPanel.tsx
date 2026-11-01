import { useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { FLAGS } from "@/lib/flags";
import { preferences, usePreferences, useRelaunchPending } from "@/preferences";
import { ChoiceRow } from "../agent/editors";
import { Group, Row } from "../agent/rows";
import { Hint } from "../Hint";

const SEARCH_FROM = 6;

export function ExperimentalPanel() {
  const settings = usePreferences((p) => p.flags);
  const relaunchPending = useRelaunchPending();
  const [search, setSearch] = useState("");
  const words = search.trim().toLowerCase();
  const shown = words
    ? FLAGS.filter((f) => `${f.name} ${f.description}`.toLowerCase().includes(words))
    : FLAGS;
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
      {relaunchPending && (
        <Group>
          <Row
            label="Relaunch to apply"
            description="Some of your changes take effect after the app relaunches."
          >
            <Button variant="brand" onClick={() => window.location.reload()}>
              Relaunch
            </Button>
          </Row>
        </Group>
      )}
      {FLAGS.length >= SEARCH_FROM && (
        <Input
          className="mb-6"
          aria-label="Search experimental features"
          placeholder="Search by name or description"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
      )}
      {shown.length > 0 ? (
        <Group>
          {shown.map((f) => (
            <ChoiceRow
              key={f.id}
              id={`settings-flag-${f.id}`}
              label={f.name}
              description={
                f.restart ? (
                  <>
                    {f.description}
                    <br />
                    Takes effect after relaunch.
                  </>
                ) : (
                  f.description
                )
              }
              value={settings[f.id] ?? "default"}
              onChange={(v) => preferences.setFlag(f.id, v as "default" | "on" | "off")}
              items={[
                { value: "default", label: `Default (${f.default ? "on" : "off"})` },
                { value: "on", label: "Enabled" },
                { value: "off", label: "Disabled" },
              ]}
            />
          ))}
        </Group>
      ) : (
        <p className="text-sm text-muted-foreground">No feature matches “{search}”.</p>
      )}
    </div>
  );
}

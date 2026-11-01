import type { ThemePreference } from "@/theme";
import { useEffect, useRef, useState } from "react";
import {
  productTelemetryStatus,
  setProductTelemetry,
  type ProductTelemetryStatus,
} from "../../../lib/nativeShell";
import type { FollowUp, NavMode, SentTurnPlacement } from "@/preferences";
import {
  preferences,
  useFlag,
  useFollowUp,
  useNavMode,
  useSentTurnPlacement,
  useThemePreference,
} from "@/preferences";
import { MOD } from "../Shortcuts";
import { Button } from "@gents/ui/components/button";
import { openCustomizeNav } from "@/app/CustomizeNav";
import { Monitor, Moon, Sun } from "lucide-react";
import { ToggleGroup, ToggleGroupItem } from "@gents/ui/components/toggle-group";
import { Hint } from "../Hint";
import { ChoiceRow } from "../agent/editors";
import { Group, Row } from "../agent/rows";

const THEMES: { value: ThemePreference; label: string; icon: typeof Sun }[] = [
  { value: "system", label: "System", icon: Monitor },
  { value: "light", label: "Light", icon: Sun },
  { value: "dark", label: "Dark", icon: Moon },
];

/* the three themes side by side, the chosen one filled; the arrow keys
   move between them */
function ThemeChoice({ value }: { value: ThemePreference }) {
  return (
    <ToggleGroup
      aria-label="Theme"
      value={[value]}
      onValueChange={(next) => {
        const chosen = next[0] as ThemePreference | undefined;
        if (chosen) preferences.setTheme(chosen);
      }}
      className="shrink-0 gap-0.5 rounded-xl bg-muted/60 p-0.5"
    >
      {THEMES.map(({ value: theme, label, icon: Icon }) => (
        <Hint key={theme} label={label}>
          <ToggleGroupItem
            value={theme}
            aria-label={label}
            className="size-7 rounded-lg text-muted-foreground hover:bg-transparent hover:text-foreground aria-pressed:bg-background aria-pressed:text-foreground aria-pressed:shadow-sm"
          >
            <Icon className="size-4" />
          </ToggleGroupItem>
        </Hint>
      ))}
    </ToggleGroup>
  );
}

export function GeneralPanel() {
  const theme = useThemePreference();
  const nav = useNavMode();
  const sentTurn = useSentTurnPlacement();
  const followUp = useFollowUp();
  const customizableNav = useFlag("customize-nav");
  const [usageTelemetry, setUsageTelemetry] = useState<ProductTelemetryStatus | null>(
    null,
  );
  const [telemetryError, setTelemetryError] = useState(false);
  const [telemetrySaving, setTelemetrySaving] = useState(false);
  const telemetryPending = useRef(false);
  useEffect(() => {
    void productTelemetryStatus()
      .then(setUsageTelemetry)
      .catch(() => setTelemetryError(true));
  }, []);
  return (
    <div>
      <Group title="Appearance">
        <Row
          label="Theme"
          description="Follow the system, or always light or dark, for every window of the app."
        >
          <ThemeChoice value={theme} />
        </Row>
        <ChoiceRow
          id="settings-nav"
          label="Side nav"
          description="How the navigation panel next to the rail shows on a wide window."
          value={nav}
          onChange={(v) => preferences.setNav(v as NavMode)}
          items={[
            { value: "hover", label: "Open on hover" },
            { value: "expanded", label: "Always open" },
            { value: "collapsed", label: "Icons only" },
          ]}
        />
        {customizableNav && (
          <Row
            label="Navigation items"
            description="Reorder the side nav's items, hide them, or add pages of the node to it."
          >
            <Button variant="outline" size="sm" onClick={openCustomizeNav}>
              Customize…
            </Button>
          </Row>
        )}
      </Group>
      <Group title="Privacy">
        {usageTelemetry?.configured || usageTelemetry?.enabled ? (
          <ChoiceRow
            id="settings-usage-telemetry"
            label="Usage telemetry"
            description={
              usageTelemetry.configured
                ? "Share app opens with the configured analytics service on the next launch. Your network address is visible to that service."
                : "Usage telemetry is not configured. You can still turn off the saved preference."
            }
            value={usageTelemetry.enabled ? "on" : "off"}
            disabled={telemetrySaving}
            onChange={(value) => {
              if (
                telemetryPending.current ||
                (value === "on" && !usageTelemetry.configured)
              )
                return;
              telemetryPending.current = true;
              setTelemetrySaving(true);
              setTelemetryError(false);
              void setProductTelemetry(value === "on")
                .then(setUsageTelemetry)
                .catch(async () => {
                  setTelemetryError(true);
                  try {
                    setUsageTelemetry(await productTelemetryStatus());
                  } catch {}
                })
                .finally(() => {
                  telemetryPending.current = false;
                  setTelemetrySaving(false);
                });
            }}
            items={[
              { value: "off", label: "Off" },
              { value: "on", label: "On" },
            ]}
          />
        ) : (
          <p>
            {usageTelemetry
              ? "Usage telemetry is not configured."
              : telemetryError
                ? "Usage telemetry status is unavailable."
                : "Checking usage telemetry…"}
          </p>
        )}
        {telemetryError && (
          <p>
            Could not save or read the telemetry preference. Unsaved changes may be lost
            after restart.
          </p>
        )}
      </Group>
      <Group title="Sessions">
        <ChoiceRow
          id="settings-sent-turn"
          label="After sending"
          description="Where the transcript goes while a reply fills in."
          value={sentTurn}
          onChange={(v) => preferences.setSentTurn(v as SentTurnPlacement)}
          items={[
            { value: "follow", label: "Follow the reply" },
            { value: "anchor", label: "Keep my message at the top" },
          ]}
        />
        <ChoiceRow
          id="settings-follow-up"
          label="Follow-up behavior"
          description={
            <>
              What a message sent while the agent works does.
              <br />
              {MOD}↵ sends one message the other way.
            </>
          }
          value={followUp}
          onChange={(v) => preferences.setFollowUp(v as FollowUp)}
          items={[
            { value: "steer", label: "Steer the running turn" },
            { value: "queue", label: "Queue for after it" },
          ]}
        />
      </Group>
    </div>
  );
}

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
  useFollowUp,
  useNavMode,
  useSentTurnPlacement,
  useTheme,
} from "@/preferences";
import { MOD } from "../Shortcuts";
import { ChoiceRow } from "../agent/editors";
import { Group } from "../agent/rows";

export function GeneralPanel() {
  const theme = useTheme();
  const nav = useNavMode();
  const sentTurn = useSentTurnPlacement();
  const followUp = useFollowUp();
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
        <ChoiceRow
          id="settings-theme"
          label="Theme"
          description="Light or dark, for every window of the app."
          value={theme}
          onChange={(v) => preferences.setTheme(v as ThemePreference)}
          items={[
            { value: "light", label: "Light" },
            { value: "dark", label: "Dark" },
          ]}
        />
        <ChoiceRow
          id="settings-nav"
          label="Side nav"
          description="How the navigation panel next to the rail shows on a wide window."
          value={nav}
          onChange={(v) => preferences.setNav(v as NavMode)}
          items={[
            { value: "hover", label: "Show on hover" },
            { value: "expanded", label: "Always expanded" },
            { value: "collapsed", label: "Collapsed" },
          ]}
        />
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

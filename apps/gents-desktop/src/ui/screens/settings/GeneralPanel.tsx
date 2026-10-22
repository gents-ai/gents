import type { ThemePreference } from "@/theme";
import type { NavMode, SentTurnPlacement } from "@/preferences";
import { preferences, useNavMode, useSentTurnPlacement, useTheme } from "@/preferences";
import { ChoiceRow } from "../agent/editors";
import { Group } from "../agent/rows";

export function GeneralPanel() {
  const theme = useTheme();
  const nav = useNavMode();
  const sentTurn = useSentTurnPlacement();
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
      </Group>
    </div>
  );
}

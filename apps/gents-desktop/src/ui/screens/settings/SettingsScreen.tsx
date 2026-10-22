import { href, navigate } from "@/lib/router";
import { SectionedScreen } from "../SectionedScreen";
import { DEFAULT_SETTINGS_SECTION, SETTINGS_SECTIONS } from "./sections";

export function SettingsScreen({ section }: { section?: string }) {
  const current =
    SETTINGS_SECTIONS.find((s) => s.id === section) ??
    SETTINGS_SECTIONS.find((s) => s.id === DEFAULT_SETTINGS_SECTION)!;
  return (
    <SectionedScreen
      entries={SETTINGS_SECTIONS}
      current={current.id}
      hrefFor={(id) => href({ name: "settings", section: id })}
      onSelect={(id) => navigate({ name: "settings", section: id })}
      scrollKey={current.id}
      testId="settings-screen"
    >
      <current.Panel />
    </SectionedScreen>
  );
}

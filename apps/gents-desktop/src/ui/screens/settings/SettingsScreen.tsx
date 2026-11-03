import { href, navigate } from "@/lib/router";
import { SectionedScreen } from "../SectionedScreen";
import { DEFAULT_SETTINGS_SECTION, useSettingsSections } from "./sections";

export function SettingsScreen({ section }: { section?: string }) {
  const sections = useSettingsSections();
  /* a section behind a feature that is off falls back to General */
  const current =
    sections.find((s) => s.id === section) ??
    sections.find((s) => s.id === DEFAULT_SETTINGS_SECTION)!;
  return (
    <SectionedScreen
      entries={sections}
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

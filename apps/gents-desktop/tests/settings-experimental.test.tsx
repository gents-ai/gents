import { fireEvent, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { FLAGS, type Flag, type FlagId } from "../src/ui/lib/flags";
import { flag } from "../src/ui/preferences";
import { SettingsScreen } from "../src/ui/screens/settings/SettingsScreen";
import { SETTINGS_SECTIONS } from "../src/ui/screens/settings/sections";
import { renderIn, testApp, withFlags } from "./app-fixture";

/* the shipped list has no flag that applies at launch, so this file adds
   one to cover the relaunch path */
vi.mock("../src/ui/lib/flags", async (original) => {
  const real = await original<typeof import("../src/ui/lib/flags")>();
  const probe: Flag = {
    id: "relaunch-probe",
    name: "Relaunch probe",
    description: "Turns on a test-only feature that applies at launch.",
    default: false,
    owner: "tests",
    removeBy: "2999-01-01",
    removeWhen: "Never; it exists only in this test file.",
    restart: true,
  };
  return { ...real, FLAGS: [...real.FLAGS, probe] };
});

const DEMO = FLAGS.find((f) => f.id === "demo-feature")!;
const PROBE: string = "relaunch-probe";

const openSection = () =>
  renderIn(testApp(), <SettingsScreen section="experimental" />);
const toggle = (name: string) => screen.getByRole("switch", { name });

describe("the experimental features settings", () => {
  it("sits last in the settings, as its own section", () => {
    expect(SETTINGS_SECTIONS[SETTINGS_SECTIONS.length - 1]!.id).toBe("experimental");
    openSection();
    expect(screen.getByTestId("settings-screen").dataset.section).toBe("experimental");
    expect(
      screen.getByRole("heading", { name: "Experimental features" }),
    ).toBeVisible();
    expect(
      screen.getByText(
        "Try features that aren’t finished. They may change or go away.",
      ),
    ).toBeVisible();
  });

  it("shows a row per flag with its description, at its default", () => {
    openSection();
    for (const f of FLAGS) {
      if (f.default) expect(toggle(f.name)).toBeChecked();
      else expect(toggle(f.name)).not.toBeChecked();
      expect(screen.getByTitle(f.description)).toHaveTextContent(f.description);
    }
    expect(screen.queryByText("Changed")).toBeNull();
    expect(screen.queryByRole("button", { name: /to default$/ })).toBeNull();
  });

  it("marks only a flag that applies at launch", () => {
    openSection();
    expect(screen.getAllByText("Relaunch")).toHaveLength(
      FLAGS.filter((f) => f.restart).length,
    );
  });

  it("sets an explicit choice when a switch flips, and resets it to the default", async () => {
    const user = userEvent.setup();
    openSection();
    await user.click(toggle(DEMO.name));
    expect(flag("demo-feature")).toBe(true);
    expect(toggle(DEMO.name)).toBeChecked();
    expect(screen.getByText("Changed")).toBeVisible();
    await user.click(toggle(DEMO.name));
    expect(flag("demo-feature")).toBe(false);
    expect(toggle(DEMO.name)).not.toBeChecked();
    expect(screen.getByText("Changed")).toBeVisible();
    await user.click(
      screen.getByRole("button", { name: `Reset ${DEMO.name} to default` }),
    );
    expect(flag("demo-feature")).toBe(DEMO.default);
    expect(screen.queryByText("Changed")).toBeNull();
    expect(screen.queryByRole("button", { name: /to default$/ })).toBeNull();
  });

  it("clears every choice with Reset all", async () => {
    const user = userEvent.setup();
    withFlags({ "demo-feature": true, [PROBE]: true });
    openSection();
    expect(screen.getAllByText("Changed")).toHaveLength(2);
    const reset = screen.getByRole("button", { name: "Reset all" });
    await user.click(reset);
    expect(flag("demo-feature")).toBe(false);
    expect(flag(PROBE as FlagId)).toBe(false);
    expect(screen.queryByText("Changed")).toBeNull();
    expect(reset).toBeDisabled();
  });

  it("asks the system not to autocorrect the search", () => {
    openSection();
    const search = screen.getByRole("searchbox", {
      name: "Search experimental features",
    });
    expect(search).toHaveAttribute("autocorrect", "off");
    expect(search).toHaveAttribute("spellcheck", "false");
  });

  it("filters by name, description and id, and clears the search with Escape", async () => {
    const user = userEvent.setup();
    openSection();
    const search = screen.getByRole("searchbox", {
      name: "Search experimental features",
    });
    await user.type(search, "probe");
    expect(screen.queryByRole("switch", { name: DEMO.name })).toBeNull();
    expect(toggle("Relaunch probe")).toBeVisible();
    await user.clear(search);
    await user.type(search, "how an experimental");
    expect(toggle(DEMO.name)).toBeVisible();
    expect(screen.queryByRole("switch", { name: "Relaunch probe" })).toBeNull();
    await user.clear(search);
    await user.type(search, "demo-feature");
    expect(toggle(DEMO.name)).toBeVisible();
    await user.clear(search);
    await user.type(search, "nothing like this");
    expect(screen.getByText("No experimental features match")).toBeVisible();
    expect(screen.queryAllByRole("switch")).toHaveLength(0);
    fireEvent.keyDown(search, { key: "Escape" });
    expect(search).toHaveValue("");
    expect(screen.getAllByRole("switch")).toHaveLength(FLAGS.length);
  });

  it("offers a relaunch once a flag that applies at launch changes", async () => {
    const reload = vi.fn();
    const original = window.location;
    Object.defineProperty(window, "location", {
      configurable: true,
      value: { ...original, reload },
    });
    try {
      const user = userEvent.setup();
      openSection();
      await user.click(toggle(DEMO.name));
      expect(screen.queryByTestId("relaunch-bar")).toBeNull();
      await user.click(toggle("Relaunch probe"));
      const bar = screen.getByTestId("relaunch-bar");
      expect(bar).toHaveClass("sticky");
      expect(bar).toHaveTextContent("Some changes apply after the app relaunches");
      expect(
        toggle("Relaunch probe").compareDocumentPosition(bar) &
          Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
      await user.click(within(bar).getByRole("button", { name: "Relaunch" }));
      expect(reload).toHaveBeenCalledTimes(1);
      await user.click(toggle("Relaunch probe"));
      expect(screen.queryByTestId("relaunch-bar")).toBeNull();
      await user.click(toggle("Relaunch probe"));
      await user.click(
        screen.getByRole("button", { name: "Reset Relaunch probe to default" }),
      );
      expect(screen.queryByTestId("relaunch-bar")).toBeNull();
      await user.click(toggle("Relaunch probe"));
      expect(screen.getByTestId("relaunch-bar")).toBeVisible();
      await user.click(screen.getByRole("button", { name: "Reset all" }));
      expect(screen.queryByTestId("relaunch-bar")).toBeNull();
    } finally {
      Object.defineProperty(window, "location", {
        configurable: true,
        value: original,
      });
    }
  });

  it("asks for no relaunch for what the app launched with", () => {
    withFlags({ [PROBE]: true });
    openSection();
    expect(screen.queryByTestId("relaunch-bar")).toBeNull();
    expect(toggle("Relaunch probe")).toBeChecked();
  });
});

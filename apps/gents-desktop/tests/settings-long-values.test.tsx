import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ChipsRow,
  FactRow,
  LongFactRow,
  TagsRow,
} from "../src/ui/screens/agent/editors";

const long = "a-skill-whose-name-runs-well-past-the-width-of-one-chip";

describe("long values in settings rows", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("bounds a long selection of chips and keeps each name whole in its title", () => {
    render(
      <ChipsRow
        id="skills"
        label="Skills"
        value={[long, "short"]}
        onChange={() => {}}
        items={[
          { value: long, label: long },
          { value: "short", label: "short" },
        ]}
      />,
    );
    const name = screen.getByTitle(long);
    expect(name).toHaveTextContent(long);
    expect(name.className).toContain("truncate");
    expect(name.closest("[data-slot=combobox-chip]")!.className).toContain("max-w-48");
    expect(name.closest("[data-slot=combobox-chips]")!.className).toContain(
      "overflow-y-auto",
    );
  });

  it("bounds a long list of tags and keeps each tag whole in its title", () => {
    render(<TagsRow id="tags" label="Tags" value={[long]} onChange={() => {}} />);
    const tag = screen.getByTitle(long);
    expect(tag.className).toContain("truncate");
    expect(tag.parentElement!.className).toContain("max-w-48");
    expect(tag.parentElement!.parentElement!.className).toContain("overflow-y-auto");
  });

  it("titles a text fact with its whole value", () => {
    render(<FactRow label="Endpoint">{long}</FactRow>);
    expect(screen.getByText(long)).toHaveAttribute("title", long);
  });

  it("wraps a long error under its label and offers Show all once it overflows", async () => {
    /* jsdom lays nothing out: this observer reports the clamped text's size */
    const observers = new Set<() => void>();
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(private readonly callback: () => void) {}
        observe() {
          observers.add(this.callback);
        }
        disconnect() {
          observers.delete(this.callback);
        }
      },
    );
    const error = "request failed: ".repeat(40);
    render(<LongFactRow label="Last error">{error}</LongFactRow>);
    const text = screen.getByText(error.trim());
    expect(text.className).toContain("line-clamp-3");
    expect(screen.queryByRole("button", { name: "Show all" })).toBeNull();

    Object.defineProperties(text, {
      scrollHeight: { configurable: true, get: () => 200 },
      clientHeight: { configurable: true, get: () => 60 },
    });
    act(() => observers.forEach((notify) => notify()));
    await userEvent.click(screen.getByRole("button", { name: "Show all" }));
    expect(text.className).not.toContain("line-clamp-3");
    expect(screen.getByRole("button", { name: "Show less" })).toBeVisible();
  });
});

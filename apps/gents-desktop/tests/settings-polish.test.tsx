import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ChipsRow, TagsRow } from "../src/ui/screens/agent/editors";
import { BehaviorHoverCard } from "../src/ui/screens/HoverCards";
import { deployment } from "./config-panel-wiring/fixtures";

describe("settings polish", () => {
  it("shows canonical skill picks as removable chips", () => {
    render(
      <ChipsRow
        id="context-skills"
        label="Skills"
        value={["skill-a"]}
        onChange={vi.fn()}
        items={[{ value: "skill-a", label: "Skill A" }]}
      />,
    );

    expect(screen.getByText("Skill A")).toBeVisible();
    expect(screen.getByRole("button", { name: "Remove Skill A" })).toBeVisible();
  });

  it("adds and removes free-form tags without changing their canonical values", async () => {
    const onChange = vi.fn();
    const view = render(
      <TagsRow
        id="behavior-tags"
        label="Tags"
        value={["existing"]}
        onChange={onChange}
      />,
    );
    const user = userEvent.setup();

    await user.type(screen.getByRole("textbox", { name: "Tags" }), "new-tag,");
    expect(onChange).toHaveBeenLastCalledWith(["existing", "new-tag"]);

    view.rerender(
      <TagsRow
        id="behavior-tags"
        label="Tags"
        value={["existing", "new-tag"]}
        onChange={onChange}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Remove existing" }));
    expect(onChange).toHaveBeenLastCalledWith(["new-tag"]);
  });

  it("deduplicates tags within one pasted value", async () => {
    const onChange = vi.fn();
    render(<TagsRow id="behavior-tags" label="Tags" value={[]} onChange={onChange} />);
    const user = userEvent.setup();
    const input = screen.getByRole("textbox", { name: "Tags" });

    await user.click(input);
    await user.paste("coding,coding");

    expect(onChange).toHaveBeenCalledOnce();
    expect(onChange).toHaveBeenCalledWith(["coding"]);
  });

  it("explains behavior readiness and links to the canonical settings route", async () => {
    render(
      <BehaviorHoverCard deployment={deployment} behaviorId="default">
        <button type="button">Default behavior</button>
      </BehaviorHoverCard>,
    );
    const user = userEvent.setup();

    await user.hover(screen.getByRole("button", { name: "Default behavior" }));
    const card = await screen.findByTestId("behaviour-hover-card");
    expect(card).toHaveTextContent("StatusEnabled");
    expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute(
      "href",
      "/agents/did%3Akey%3Az6MkAgent/behaviors/default",
    );
  });
});

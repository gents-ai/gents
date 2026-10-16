import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AreaRow } from "../src/ui/screens/agent/editors";

describe("a text area beside its label", () => {
  it("caps a self-sizing text area's height", () => {
    render(
      <AreaRow
        id="description"
        label="Description"
        value={"line\n".repeat(60)}
        onChange={() => {}}
      />,
    );
    expect(screen.getByLabelText("Description")).toHaveClass("max-h-48");
  });
});

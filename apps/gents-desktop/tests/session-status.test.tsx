import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SessionStatus } from "../src/ui/screens/SessionStatus";

describe("a session's status mark", () => {
  it("says working for a live state this build does not name, as its spinner does", () => {
    render(<SessionStatus turnState="compacting" />);
    expect(screen.getByRole("img", { name: "Working" })).toBeInTheDocument();
  });

  it("says finished for a settled turn", () => {
    render(<SessionStatus turnState="completed" />);
    expect(screen.getByRole("img", { name: "Finished" })).toBeInTheDocument();
  });
});

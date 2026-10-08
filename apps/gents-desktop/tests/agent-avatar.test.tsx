import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { AgentAvatar } from "../src/ui/screens/AgentAvatar";

describe("an agent avatar", () => {
  it("falls back to initials when its picture fails, and tries the next agent's picture", () => {
    const view = render(
      <AgentAvatar name="Ada Lovelace" src="/a.png" data-testid="avatar" />,
    );
    fireEvent.error(screen.getByTestId("avatar"));
    expect(screen.getByTestId("avatar")).toHaveTextContent("Al");

    view.rerender(
      <AgentAvatar name="Grace Hopper" src="/g.png" data-testid="avatar" />,
    );
    expect(screen.getByTestId("avatar").tagName).toBe("IMG");
    expect(screen.getByTestId("avatar")).toHaveAttribute("src", "/g.png");
  });
});

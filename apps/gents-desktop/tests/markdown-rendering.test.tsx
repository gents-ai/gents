import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const markdownParse = vi.hoisted(() => vi.fn());

vi.mock("react-markdown", () => ({
  default: ({ children }: { children: string }) => {
    markdownParse(children);
    return <div>{children}</div>;
  },
}));

import { Markdown } from "../src/ui/screens/Markdown";

describe("Markdown render boundary", () => {
  it("does not parse unchanged content again", () => {
    markdownParse.mockClear();
    const view = render(<Markdown>stable markdown</Markdown>);

    expect(markdownParse).toHaveBeenCalledTimes(1);
    view.rerender(<Markdown>stable markdown</Markdown>);
    expect(markdownParse).toHaveBeenCalledTimes(1);

    view.rerender(<Markdown>changed markdown</Markdown>);
    expect(markdownParse).toHaveBeenCalledTimes(2);
  });
});

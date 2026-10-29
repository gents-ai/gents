import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SentAttachments } from "../src/ui/screens/session/attachments";

vi.mock("../src/hooks/useAttachments", () => ({
  useSentAttachmentPreviews: () => ({
    root: { current: null },
    previews: {},
    error: null,
    retry: vi.fn(),
  }),
}));

describe("queued edit attachments", () => {
  it("removes by original index after an earlier attachment is hidden", () => {
    const remove = vi.fn();
    render(
      <SentAttachments
        items={[
          { name: "first.txt", mediaType: "text/plain", digest: "first", size: 1 },
          { name: "second.txt", mediaType: "text/plain", digest: "second", size: 2 },
        ]}
        removed={[0]}
        onRemove={remove}
      />,
    );
    expect(screen.queryByText("first.txt")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Remove second.txt" }));
    expect(remove).toHaveBeenCalledWith(1);
  });
});

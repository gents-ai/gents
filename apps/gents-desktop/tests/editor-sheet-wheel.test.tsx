import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { EditorSheet } from "../src/ui/screens/agent/EditorSheet";

/* a sheet opened from inside another, as a behavior's profile is */
function Stacked({
  onCloseBelow = () => {},
  onCloseTop = () => {},
}: {
  onCloseBelow?: () => void;
  onCloseTop?: () => void;
}) {
  return (
    <EditorSheet open onClose={onCloseBelow} title="Behavior">
      <p>behavior fields</p>
      <EditorSheet open onClose={onCloseTop} title="Profile">
        <p>profile fields</p>
      </EditorSheet>
    </EditorSheet>
  );
}

function wheel(target: Element) {
  const event = new WheelEvent("wheel", {
    deltaY: 120,
    bubbles: true,
    cancelable: true,
  });
  target.dispatchEvent(event);
  return event;
}

/* jsdom does not scroll; record which element was asked to */
function watchScrolls() {
  const scrollBy = vi.fn();
  Object.defineProperty(HTMLElement.prototype, "scrollBy", {
    value: scrollBy,
    configurable: true,
  });
  return scrollBy;
}

describe("stacked editor sheets", () => {
  afterEach(() => {
    Reflect.deleteProperty(HTMLElement.prototype, "scrollBy");
  });

  it("leave a wheel over the top sheet to scroll that sheet", () => {
    const scrollBy = watchScrolls();
    render(<Stacked />);
    wheel(screen.getByText("profile fields"));
    expect(scrollBy).not.toHaveBeenCalled();
  });

  it("send a wheel over the backdrop to the top sheet only", () => {
    const scrollBy = watchScrolls();
    render(<Stacked />);
    const overlays = document.querySelectorAll("[data-slot=sheet-overlay]");
    /* each stacked sheet draws its own overlay, the top one last */
    expect(overlays).toHaveLength(2);
    wheel(overlays[1]!);
    expect(scrollBy).toHaveBeenCalledTimes(1);
    expect(scrollBy.mock.contexts[0]).toBe(
      screen.getByText("profile fields").closest("[role=dialog] .overflow-y-auto"),
    );
  });

  it("leave a wheel over a list the sheet opened to scroll that list", () => {
    const scrollBy = watchScrolls();
    render(
      <EditorSheet open onClose={() => {}} title="Behavior">
        <Select defaultOpen defaultValue="a">
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="a">alpha</SelectItem>
            <SelectItem value="b">beta</SelectItem>
          </SelectContent>
        </Select>
      </EditorSheet>,
    );
    wheel(screen.getByRole("listbox"));
    expect(scrollBy).not.toHaveBeenCalled();
  });

  it("close the topmost sheet, and only it, on a press on the backdrop", async () => {
    const onCloseBelow = vi.fn();
    const onCloseTop = vi.fn();
    render(<Stacked onCloseBelow={onCloseBelow} onCloseTop={onCloseTop} />);
    const overlays = document.querySelectorAll("[data-slot=sheet-overlay]");
    /* the topmost overlay is the one a press over the backdrop lands on */
    const backdrop = overlays[overlays.length - 1]!;
    await act(async () => {
      fireEvent.pointerDown(backdrop, { button: 0, pointerType: "mouse" });
      fireEvent.mouseDown(backdrop, { button: 0 });
      fireEvent.pointerUp(backdrop, { button: 0, pointerType: "mouse" });
      fireEvent.mouseUp(backdrop, { button: 0 });
      fireEvent.click(backdrop, { button: 0 });
    });
    expect(onCloseTop).toHaveBeenCalledOnce();
    expect(onCloseBelow).not.toHaveBeenCalled();
  });
});

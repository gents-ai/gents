import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { NodeAvatar } from "../src/ui/screens/AgentAvatar";
import { AvatarRoll } from "../src/ui/screens/AvatarRoll";
import { settleAvatar } from "@/lib/avatarRoll";

describe("a node avatar", () => {
  it("falls back to initials when its picture fails, and tries the next node's picture", () => {
    const view = render(
      <NodeAvatar name="Ada Lovelace" src="/a.png" data-testid="avatar" />,
    );
    fireEvent.error(screen.getByTestId("avatar").querySelector("img")!);
    expect(screen.getByTestId("avatar")).toHaveTextContent("Al");

    view.rerender(<NodeAvatar name="Grace Hopper" src="/g.png" data-testid="avatar" />);
    expect(screen.getByTestId("avatar").querySelector("img")).toHaveAttribute(
      "src",
      "/g.png",
    );
  });

  it("draws its chosen portrait", () => {
    render(<NodeAvatar name="Forge" avatar="12" data-testid="avatar" />);
    expect(screen.getByTestId("avatar").querySelector("img")).toHaveAttribute(
      "src",
      "/avatars/12.png",
    );
  });
});

describe("rolling a node's avatar", () => {
  const decode = vi.fn(() => Promise.resolve());
  beforeEach(() => {
    vi.stubGlobal(
      "Image",
      class {
        src = "";
        decode = decode;
      },
    );
    vi.stubGlobal("requestAnimationFrame", (run: FrameRequestCallback) => {
      run(0);
      return 0;
    });
  });
  /* the test DOM has no Web Animations; each reel's landing is called here */
  const landings: (() => void)[] = [];
  const animate = Element.prototype.animate;
  beforeEach(() => {
    Element.prototype.animate = function () {
      const animation = { onfinish: null as (() => void) | null, cancel() {} };
      landings.push(() => animation.onfinish?.());
      return animation as unknown as Animation;
    };
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    Element.prototype.animate = animate;
    landings.length = 0;
  });
  const land = () => act(() => landings.splice(0).forEach((landing) => landing()));

  const reelOf = (testId: string) =>
    [...screen.getByTestId(testId).querySelectorAll("img")].map((img) =>
      img.getAttribute("src"),
    );

  it("spins every avatar of the node in step and shows the new one until settled", async () => {
    const onChange = vi.fn();
    render(
      <>
        <AvatarRoll rollId="did:roll:a" name="Forge" avatar="3" onChange={onChange} />
        <NodeAvatar name="Forge" avatar="3" rollId="did:roll:a" data-testid="rail" />
        <NodeAvatar name="Other" avatar="3" rollId="did:roll:b" data-testid="other" />
      </>,
    );
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Roll a new avatar" }));
      await new Promise((settle) => setTimeout(settle, 0));
    });

    const page = screen.getByRole("button", { name: "Roll a new avatar" });
    const pageReel = [...page.querySelectorAll("img")].map((img) =>
      img.getAttribute("src"),
    );
    const railReel = reelOf("rail");
    expect(railReel.length).toBeGreaterThan(2);
    expect(railReel).toEqual(pageReel);
    expect(railReel[0]).toBe("/avatars/3.png");
    const landed = railReel[railReel.length - 1]!;
    expect(landed).not.toBe("/avatars/3.png");
    expect(onChange).toHaveBeenCalledWith(landed.replace(/\D/g, ""));
    expect(reelOf("other")).toEqual(["/avatars/3.png"]);

    land();
    expect(reelOf("rail").at(-1)).toBe(landed);

    act(() => settleAvatar("did:roll:a"));
    expect(reelOf("rail")).toEqual(["/avatars/3.png"]);
  });

  it("rolls again mid-roll, from the one it was rolling to", async () => {
    const onChange = vi.fn();
    render(
      <>
        <AvatarRoll rollId="did:roll:e" name="Forge" avatar="6" onChange={onChange} />
        <NodeAvatar name="Forge" avatar="6" rollId="did:roll:e" data-testid="rail" />
      </>,
    );
    const roll = screen.getByRole("button", { name: "Roll a new avatar" });
    const landing = () => reelOf("rail").at(-1)!.replace(/\D/g, "");
    await act(async () => {
      fireEvent.click(roll);
      await new Promise((settle) => setTimeout(settle, 0));
    });
    const first = landing();
    await act(async () => {
      fireEvent.click(roll);
      await new Promise((settle) => setTimeout(settle, 0));
    });
    expect(reelOf("rail")[0]).toBe(`/avatars/${first}.png`);
    expect(onChange.mock.calls.map(([avatar]) => avatar)).toEqual([first, landing()]);
  });
});

import { render } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

import { useNow } from "../src/ui/lib/clock";

afterEach(() => vi.restoreAllMocks());

it("settles while the time moves on between a render and its subscription", () => {
  let t = 1_000_000;
  vi.spyOn(Date, "now").mockImplementation(() => (t += 1));
  let renders = 0;
  function Ticking() {
    renders += 1;
    return <p>{useNow(true)}</p>;
  }
  const { unmount } = render(<Ticking />);
  expect(renders).toBeLessThan(5);
  unmount();
});

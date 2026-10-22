import { beforeEach, describe, expect, it } from "vitest";
import {
  PLACES_KEPT,
  resetSessionViews,
  restoreSessionViews,
  sessionViews,
} from "../src/ui/app/sessionViews";

const at = (sequence: number) => ({ key: `row-${sequence}`, offset: 40, sequence });

describe("how each session was last seen", () => {
  beforeEach(resetSessionViews);

  it("returns the place saved for a session, and forgets it at the foot", () => {
    sessionViews.setPlace("a", at(3));
    sessionViews.setPlace("b", at(9));
    expect(sessionViews.placeOf("a")).toEqual(at(3));
    sessionViews.setPlace("a", null);
    expect(sessionViews.placeOf("a")).toBeNull();
    expect(sessionViews.placeOf("b")).toEqual(at(9));
  });

  it("keeps the sessions read most recently and drops the rest", () => {
    for (let n = 0; n <= PLACES_KEPT; n += 1) sessionViews.setPlace(`s${n}`, at(n));
    expect(sessionViews.placeOf("s0")).toBeNull();
    sessionViews.setPlace("s1", at(1));
    sessionViews.setPlace("new", at(0));
    expect(sessionViews.placeOf("s1")).toEqual(at(1));
  });

  it("restores only places it can use", () => {
    expect(restoreSessionViews("nonsense").places).toEqual([]);
    expect(
      restoreSessionViews({
        places: [
          { session: "a", place: { key: 1 } },
          { session: "b", place: at(2) },
        ],
      }).places,
    ).toEqual([{ session: "b", place: at(2) }]);
  });
});

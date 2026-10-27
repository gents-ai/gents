import { describe, expect, it } from "vitest";
import { matchScore, queryWords } from "../src/ui/lib/search";

const score = (query: string, text: string) =>
  matchScore(queryWords(query), text.toLowerCase());

describe("search matching", () => {
  it("ranks a prefix over a word start over anywhere inside", () => {
    expect(score("rel", "Release check")).toBeGreaterThan(
      score("rel", "The release check"),
    );
    expect(score("rel", "The release check")).toBeGreaterThan(
      score("rel", "Unreleased"),
    );
    expect(score("rel", "Unreleased")).toBeGreaterThan(0);
  });

  it("needs every word, in any order, and ignores case", () => {
    expect(score("check REL", "Release check")).toBeGreaterThan(0);
    expect(score("release docs", "Release check")).toBe(0);
    expect(score("  ", "Release check")).toBe(0);
  });
});

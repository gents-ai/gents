import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { AVATARS, avatarFor, randomAvatar, shownAvatar } from "@/lib/avatar";

describe("node avatars", () => {
  it("maps names deterministically across the complete asset set", () => {
    const names = Array.from({ length: 4096 }, (_, index) => `node-${index}`);
    const firstPass = names.map(avatarFor);

    expect(names.map(avatarFor)).toEqual(firstPass);
    expect(new Set(firstPass)).toEqual(
      new Set(
        Array.from({ length: AVATARS }, (_, index) => `/avatars/${index + 1}.png`),
      ),
    );
  });

  it("draws a chosen portrait, and the name's for anything it doesn't know", () => {
    expect(shownAvatar("12", "Forge")).toBe("12");
    for (const unknown of [null, undefined, "", "0", String(AVATARS + 1), "x"])
      expect(shownAvatar(unknown, "Forge")).toBe(shownAvatar(null, "Forge"));
  });

  it("rolls to a different portrait", () => {
    for (let i = 0; i < 200; i++) expect(randomAvatar("7")).not.toBe("7");
  });

  it("ships every avatar at the intended source resolution", async () => {
    const images = await Promise.all(
      Array.from({ length: AVATARS }, (_, index) =>
        readFile(resolve("public", "avatars", `${index + 1}.png`)),
      ),
    );

    for (const image of images) {
      expect(image.subarray(1, 4).toString()).toBe("PNG");
      expect(image.readUInt32BE(16)).toBe(224);
      expect(image.readUInt32BE(20)).toBe(224);
    }
  });
});

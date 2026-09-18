import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { avatarFor } from "@/lib/avatar";

describe("agent avatars", () => {
  it("maps names deterministically across the complete asset set", () => {
    const names = Array.from({ length: 512 }, (_, index) => `agent-${index}`);
    const firstPass = names.map(avatarFor);

    expect(names.map(avatarFor)).toEqual(firstPass);
    expect(new Set(firstPass)).toEqual(
      new Set(Array.from({ length: 16 }, (_, index) => `/avatars/${index + 1}.png`)),
    );
  });

  it("ships every avatar at the intended source resolution", async () => {
    const images = await Promise.all(
      Array.from({ length: 16 }, (_, index) =>
        readFile(resolve("public", "avatars", `${index + 1}.png`)),
      ),
    );

    for (const image of images) {
      expect(image.subarray(1, 4).toString()).toBe("PNG");
      expect(image.readUInt32BE(16)).toBe(128);
      expect(image.readUInt32BE(20)).toBe(128);
    }
  });
});

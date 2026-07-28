import { gzipSync } from "fflate";
import { describe, expect, it } from "vitest";

import { decodePairingQrPayload } from "@source-inc/gents-desktop-fleet";

const compactMagic = new Uint8Array([
  0x64, 0x61, 0x62, 0x65, 0x61, 0x72, 0x31, 0x7a, 0x00,
]);

describe("pairing QR payloads", () => {
  it("accepts the legacy text QR", () => {
    expect(
      decodePairingQrPayload({
        binaryData: [],
        data: "  dabear1-signed-token  ",
      }),
    ).toBe("dabear1-signed-token");
  });

  it("reconstructs a normal bearer token from the compact binary QR", () => {
    const cbor = new Uint8Array([0, 0, 1, 2, 3, 255]);
    const compressed = gzipSync(cbor, { level: 9, mtime: 0 });
    const binaryData = [...compactMagic, ...compressed];

    expect(decodePairingQrPayload({ binaryData, data: "" })).toBe("dabear1-112VfYr");
  });

  it("ignores unrelated QR codes", () => {
    expect(
      decodePairingQrPayload({
        binaryData: [1, 2, 3],
        data: "https://example.com",
      }),
    ).toBeNull();
  });

  it("rejects compact payloads whose expanded CBOR exceeds the safety limit", () => {
    const compressed = gzipSync(new Uint8Array(16 * 1024 + 1), {
      level: 9,
      mtime: 0,
    });

    expect(
      decodePairingQrPayload({
        binaryData: [...compactMagic, ...compressed],
        data: "",
      }),
    ).toBeNull();
  });

  it("rejects malformed compact gzip payloads", () => {
    expect(
      decodePairingQrPayload({
        binaryData: [...compactMagic, 0x1f, 0x8b, 0x00, 0x00],
        data: "",
      }),
    ).toBeNull();
  });
});

import { describe, expect, it } from "vitest";

import { decodePairingQrPayload } from "./QrScannerDialog.js";

/**
 * A real v2 payload minted by the Rust encoder
 * (`compact_bearer_qr_payload_v2` in `crates/gents-cli/src/commands/p2p/invite.rs`)
 * from that module's `bearer_token()` fixture, with `schema_digest` set.
 *
 * This is the cross-language contract: the scanner in this package is a
 * SECOND, independent implementation of a wire format whose only other
 * implementation is in Rust. Nothing but a pinned payload catches the two
 * drifting apart — a positional-array format silently misreads every field
 * after an inserted slot, and the failure surfaces as "QR scan does nothing"
 * on a phone, not as a test failure.
 *
 * To regenerate after a deliberate format change: temporarily print
 * `payload` as hex from
 * `compact_bearer_qr_v2_carries_the_schema_digest_through_the_round_trip`.
 */
const V2_PAYLOAD_HEX =
  "646162656172327a001f8b08000000000002ffa5cabb4ec2501c80f11807e32b30f806c47aae2d132a24046d629098c2f63f377aa1a72d1cdac26ce2e21398b8bbf9163e82f1051c1c4c7c045d9df94ddff03d1eb4be4a542fd3dbde8e855932b9a47928c621a1b7797d1de7350a4145e86a9745f1a41a3777b321cdf0341e6cc76d9f730aca179448634c8010c7d8044c11430dd7678085a0810e3cfc97c8f3b9473c608498402ac3b8c2aa8db4556591580702d77563ead471be4e9c8bd9d2a6b46cc962859ba64c21b74a4b5525d2e9941596ed98435009106b9d095952a12a090ba8019a1592f1cdf3e9d7d1e7fbe8fb75f076f8f3f274d276ac76dde1a6262c4a664efa999d572197e38bf36127150f1f4b59d85aafd6e092c22e9436b059ba949b11ceaa209f97f731e4db858225fc3b1df210eb7abc8bf014793de2f7289a47fda33d45fde33dfd02b5f43cf9da010000";

/**
 * The exact `dabear1-` token Rust's `encode_bearer` produces for that same
 * fixture. Pinning the full string is deliberate: it verifies not just that
 * the payload parses, but that this package's re-encoder rebuilds the CBOR
 * map byte-for-byte as serde would — which is what the issuer's signature is
 * computed over. A near-miss (a key emitted as null instead of omitted, a
 * field read from the wrong slot) changes these bytes and fails here, instead
 * of failing signature verification on a stranger's phone.
 */
const EXPECTED_TOKEN =
  "dabear1-AaSV47aT4Lv6yTD1vt2XAMBAsQKUrogFpcgX6zDVTAGna6pcJqGmKqHwvv4mzyE37TzPswRAi8UHmYf3g12RWVar9EoCQ7uefvyp99mUDP7oedEasYwaYAjMNDr5gLoxjb8JNpQENTyQaEDtsRvZMkPT3hqytofbY6t12ZgFfRdFr7kJ3pNFJvcPrsrs7osvP4DRJiz6XnGjvNQnKg9jSkzaaaoEMHtB8wwKFpJ9dUuLY6qwDhu2cewz5j7MqWvbFs8xUByLMTJ8ajjquMZS861aJavTnrW97CyRvuyg3peq7S3ym3PB2FYXrj6FExZCRZ2kb2eDbcNS6XJ1QbGLk39Tq5NjAShpJQQ6se1bTyvE12HFBTEPHerLwsGX11Qs7M9LjLsTPqmHs4x4At22Tqq5nfVN3PZ5FqfbNYWhdNg54bQkNXeRqsTbE4GYJQCJ3BnrF5xTnhVwGZakLntyusmBvQzNEtgJ4WkWTSB1Mzv5t3EeRs1abapL72T4JFaexy3mC5wkxc7Jt2r55rKrtwzQcNBSPHSsE4da1aBXkhYAgQ3dgPr23SLGjpoLcpwXM7YZniCJQwxQUDJ5DAQXjEFk9Rwr9NKSEMZuGpZ7Pjm5JyT7G7Vkg4W9LKJuSARByVNReCAwKv6hA9SkLALu9Mc34PDrUPMsdtU68QAscas4JEUAyh8UKJpMXm6PC76PJUsrBouCfXT8d5mNibUBgjsLs6vmEPp2iPZdPxSMFbGARvx5LDecxP5hLgXQb6zTmx6FuTgdccnQUwMn7yxV5g1EzsZAiyucUqzRz4qxjbKYWJtmDnUjXxXgTTmXQNNopfuztsLhRhMdMpCSGoZVxFeiLteVgvg7vs2dkwp41MDom23xHN478J6surbGH9h7V4XmhX27PGCpE7rGyLDbc949eXtsscWB4ex44iChAHh73kzyYHS2fHYubeST9HBBvrGSbBmgoSZ7ZcX6c8525kaHV2U69AJakMKFtBJWuhCeVMR5M4rBNGPA12jSCngWWQ6WVmqtfk2oNiQxH5j7khzZDdwGYts2";

function hexToBytes(hex: string): number[] {
  const out: number[] = [];
  for (let i = 0; i < hex.length; i += 2) {
    out.push(Number.parseInt(hex.slice(i, i + 2), 16));
  }
  return out;
}

/** jsQR hands binary payloads back as `binaryData`; `data` is the lossy text view. */
function asScan(bytes: number[]) {
  return { binaryData: bytes, data: "" };
}

describe("decodePairingQrPayload", () => {
  it("decodes a v2 payload minted by the Rust encoder", () => {
    const decoded = decodePairingQrPayload(asScan(hexToBytes(V2_PAYLOAD_HEX)));

    expect(decoded).toBe(EXPECTED_TOKEN);
  });

  it("keeps the schema_digest slot aligned with the Rust layout", () => {
    // The digest rides slot 9 of the positional array; `network` and `sig`
    // shifted to 10 and 11 when it was added (#1122). A decoder still reading
    // the 11-field layout misreads `network` as the digest and fails the
    // length check — so a truncated 11-field payload must NOT decode.
    const full = hexToBytes(V2_PAYLOAD_HEX);
    expect(decodePairingQrPayload(asScan(full))).toBe(EXPECTED_TOKEN);

    // Corrupting the gzip body must fail closed (null), never throw.
    const corrupted = [...full];
    corrupted[corrupted.length - 1] ^= 0xff;
    expect(decodePairingQrPayload(asScan(corrupted))).toBeNull();
  });

  it("passes through an already-textual dabear1 token unchanged", () => {
    const text = "dabear1-abc123";
    expect(decodePairingQrPayload({ binaryData: [], data: text })).toBe(text);
  });

  it("returns null for a payload with neither magic", () => {
    expect(
      decodePairingQrPayload(asScan([1, 2, 3, 4, 5, 6, 7, 8, 9])),
    ).toBeNull();
  });
});

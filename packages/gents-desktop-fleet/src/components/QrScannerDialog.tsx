import { gunzipSync } from "fflate";
import jsQR from "jsqr";
import type { QRCode } from "jsqr";
import { useEffect, useRef, useState, type ReactNode } from "react";

const BEARER_QR_MAGIC = new Uint8Array([
  0x64, 0x61, 0x62, 0x65, 0x61, 0x72, 0x31, 0x7a, 0x00,
]);
const MAX_COMPACT_QR_GZIP_BYTES = 8 * 1024;
const MAX_COMPACT_QR_CBOR_BYTES = 16 * 1024;
const BASE58_ALPHABET =
  "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

export type QrScannerDialogProps = {
  onClose: () => void;
  onScan: (value: string) => void;
  pairingHint?: ReactNode;
};

function startsWithBytes(value: Uint8Array, prefix: Uint8Array): boolean {
  return (
    value.length >= prefix.length &&
    prefix.every((byte, index) => value[index] === byte)
  );
}

function encodeBase58(bytes: Uint8Array): string {
  let zeroCount = 0;
  while (zeroCount < bytes.length && bytes[zeroCount] === 0) zeroCount += 1;
  if (zeroCount === bytes.length) return "1".repeat(zeroCount);

  const digits = [0];
  for (let index = zeroCount; index < bytes.length; index += 1) {
    let carry = bytes[index];
    for (let digit = 0; digit < digits.length; digit += 1) {
      carry += digits[digit] * 256;
      digits[digit] = carry % 58;
      carry = Math.floor(carry / 58);
    }
    while (carry > 0) {
      digits.push(carry % 58);
      carry = Math.floor(carry / 58);
    }
  }

  return (
    "1".repeat(zeroCount) +
    digits
      .reverse()
      .map((digit) => BASE58_ALPHABET[digit])
      .join("")
  );
}

export function decodePairingQrPayload(
  result: Pick<QRCode, "binaryData" | "data">,
): string | null {
  const text = result.data.trim();
  if (text.startsWith("dabear1-")) return text;

  const bytes = Uint8Array.from(result.binaryData);
  if (!startsWithBytes(bytes, BEARER_QR_MAGIC)) return null;

  const compressed = bytes.subarray(BEARER_QR_MAGIC.length);
  if (compressed.length < 4 || compressed.length > MAX_COMPACT_QR_GZIP_BYTES) {
    return null;
  }

  const expectedSize = new DataView(
    compressed.buffer,
    compressed.byteOffset + compressed.byteLength - 4,
    4,
  ).getUint32(0, true);
  if (expectedSize > MAX_COMPACT_QR_CBOR_BYTES) return null;

  try {
    // Supplying the bounded output buffer prevents fflate from allocating the
    // untrusted gzip ISIZE advertised by a scanned QR payload.
    const cbor = gunzipSync(compressed, {
      out: new Uint8Array(expectedSize),
    });
    if (cbor.length !== expectedSize) return null;
    return `dabear1-${encodeBase58(cbor)}`;
  } catch {
    return null;
  }
}

export function QrScannerDialog({
  onClose,
  onScan,
  pairingHint,
}: QrScannerDialogProps) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const onCloseRef = useRef(onClose);
  const onScanRef = useRef(onScan);
  const [error, setError] = useState<string | null>(null);

  onCloseRef.current = onClose;
  onScanRef.current = onScan;

  useEffect(() => {
    let cancelled = false;
    let frame: number | null = null;
    let stream: MediaStream | null = null;

    async function startCamera() {
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: false,
          video: {
            facingMode: { ideal: "environment" },
            height: { ideal: 1080 },
            width: { ideal: 1920 },
          },
        });
        if (cancelled) {
          stream.getTracks().forEach((track) => track.stop());
          return;
        }

        const video = videoRef.current;
        if (!video) return;
        video.srcObject = stream;
        video.setAttribute("playsinline", "true");
        await video.play();
        frame = window.requestAnimationFrame(scanFrame);
      } catch (cause) {
        setError(
          cause instanceof Error
            ? cause.message
            : "Camera access is unavailable. Paste the invite instead.",
        );
      }
    }

    function scanFrame() {
      if (cancelled) return;
      const video = videoRef.current;
      const canvas = canvasRef.current;
      if (
        video &&
        canvas &&
        video.readyState >= HTMLMediaElement.HAVE_ENOUGH_DATA &&
        video.videoWidth > 0 &&
        video.videoHeight > 0
      ) {
        canvas.width = video.videoWidth;
        canvas.height = video.videoHeight;
        const context = canvas.getContext("2d", { willReadFrequently: true });
        if (context) {
          context.drawImage(video, 0, 0, canvas.width, canvas.height);
          const image = context.getImageData(0, 0, canvas.width, canvas.height);
          const result = jsQR(image.data, image.width, image.height, {
            // Terminal QR blocks inherit the terminal foreground/background.
            // A dark terminal therefore presents an inverted (light-on-dark)
            // code, while a light terminal presents the conventional polarity.
            inversionAttempts: "attemptBoth",
          });
          if (result) {
            try {
              const token = decodePairingQrPayload(result);
              if (token) {
                onScanRef.current(token);
                onCloseRef.current();
                return;
              }
            } catch {
              setError(
                "That pairing QR could not be decoded. Mint a fresh invite or paste its token.",
              );
            }
          }
        }
      }
      frame = window.requestAnimationFrame(scanFrame);
    }

    void startCamera();
    return () => {
      cancelled = true;
      if (frame !== null) window.cancelAnimationFrame(frame);
      stream?.getTracks().forEach((track) => track.stop());
    };
  }, []);

  return (
    <div
      aria-label="Scan pairing invite"
      aria-modal="true"
      className="fleet-qr-backdrop"
      data-testid="fleet-qr-scanner"
      role="dialog"
    >
      <section className="fleet-qr-dialog panel">
        <header>
          <div>
            <p className="eyebrow">Secure pairing</p>
            <h3>Scan agent invite</h3>
          </div>
          <button
            aria-label="Close camera"
            className="ghost-button"
            onClick={onClose}
            type="button"
          >
            Close
          </button>
        </header>
        <div className="fleet-qr-viewport">
          <video muted ref={videoRef} />
          <span aria-hidden="true" className="fleet-qr-guide" />
        </div>
        {error ? <p className="fleet-inline-error">{error}</p> : null}
        <p className="muted">
          {pairingHint ?? (
            <>
              Point the camera at the QR code printed by{" "}
              <code>gents p2p pairings invite --bearer --qr</code>.
            </>
          )}
        </p>
        <canvas aria-hidden="true" ref={canvasRef} />
      </section>
    </div>
  );
}

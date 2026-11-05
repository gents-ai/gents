import { useMemo } from "react";
import { encode } from "uqr";

/** The QR code for `text`, drawn dark on light in either theme: scanners
    read a light-on-dark code poorly, and the quiet zone around it is part
    of the code. */
export function InviteQr({ text, label }: { text: string; label: string }) {
  const { size, path } = useMemo(() => {
    const qr = encode(text, { ecc: "M", border: 4 });
    let d = "";
    qr.data.forEach((row, y) =>
      row.forEach((dark, x) => {
        if (dark) d += `M${x} ${y}h1v1h-1z`;
      }),
    );
    return { size: qr.size, path: d };
  }, [text]);
  return (
    <svg
      role="img"
      aria-label={label}
      viewBox={`0 0 ${size} ${size}`}
      shapeRendering="crispEdges"
      className="size-40 shrink-0 rounded-md bg-white"
    >
      <path d={path} fill="#000" />
    </svg>
  );
}

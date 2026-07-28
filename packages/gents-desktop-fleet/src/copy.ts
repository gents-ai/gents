import type { ReactNode } from "react";

export type FleetCopy = {
  /** Product name used in local-runtime errors. */
  runtimeProductName?: string;
  /** Binary name used in local-runtime recovery guidance. */
  cliBinaryName?: string;
  /** Host-owned QR invite instructions. */
  pairingQrHint?: ReactNode;
};

export const DEFAULT_RUNTIME_PRODUCT_NAME = "Gents";
export const DEFAULT_CLI_BINARY_NAME = "gents";

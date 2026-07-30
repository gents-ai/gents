import type { ReactNode } from "react";

export type FleetCopy = {
  runtimeProductName?: string;
  cliBinaryName?: string;
  pairingQrHint?: ReactNode;
};

export const DEFAULT_RUNTIME_PRODUCT_NAME = "Gents";
export const DEFAULT_CLI_BINARY_NAME = "gents";

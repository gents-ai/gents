import { createContext, useContext, type ReactNode } from "react";

import {
  getDesktopApiAdapter,
  type DesktopApiAdapter,
} from "@source-inc/gents-desktop-client";

const OperationsApiContext = createContext<DesktopApiAdapter | null>(null);

export type OperationsApiProviderProps = {
  api: DesktopApiAdapter;
  children: ReactNode;
};

export function OperationsApiProvider({
  api,
  children,
}: OperationsApiProviderProps) {
  return (
    <OperationsApiContext.Provider value={api}>
      {children}
    </OperationsApiContext.Provider>
  );
}

/** Resolve an explicit adapter, the nearest provider, or the legacy default. */
export function useOperationsApi(
  explicit?: DesktopApiAdapter,
): DesktopApiAdapter {
  const context = useContext(OperationsApiContext);
  return explicit ?? context ?? getDesktopApiAdapter();
}

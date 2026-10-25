export { createDesktopClient, type DesktopClient } from "./client.js";
export { decodeRuntimePreparationReport } from "./runtimePreparation.js";
export {
  createDesktopStore,
  DEFAULT_TIMING,
  type DesktopStore,
  type DesktopStoreState,
  type TimingConfig,
} from "./store.js";
export {
  tauriTransport,
  bridgeCommand,
  type DesktopTransport,
  type ClientUpdateEvent,
  type OauthProvider,
  type Unlisten,
} from "./transport.js";
export {
  BridgeInvokeError,
  asBridgeErrorPayload,
  normalizeInvokeError,
  type BridgeErrorPayload,
} from "./errors.js";
export { createDesktopApiAdapter } from "./api/adapter.js";
export type {
  DesktopApiAdapter,
  ManagedServerAuthorityInput,
  ManagedServerStatus,
  ManagedServerResetResult,
  HomeResetDisposition,
} from "./api/types.js";
export type { IncompatibleStoreView } from "./generated/IncompatibleStoreView.js";
export type { ManagedServerToolCeiling } from "./generated/ManagedServerToolCeiling.js";
export * from "./events.js";
export * from "./operationalState.js";
export * from "./turnState.js";
export * from "./types.js";

export const NARROW_BREAKPOINT_PX = 760;

export * from "./sessionLiveDelta.js";
export * from "./sessionTimeline.js";
export * from "./pendingQueue.js";

export * from "./firstRun.js";
export * from "./inferenceSetup.js";
export * from "./inferenceSettings.js";
export * from "./inferenceSetupForm.js";
export { PROVIDER_CREDENTIAL_KIND } from "./providerLogin.js";

export * from "./capturedContext.js";

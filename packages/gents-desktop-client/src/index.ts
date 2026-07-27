export {
  createDesktopClient,
  type DesktopClient,
} from "./client.js";
export {
  createDesktopStore,
  countCoalescedRefreshes,
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
  type Unlisten,
} from "./transport.js";
export {
  BridgeInvokeError,
  asBridgeErrorPayload,
  normalizeInvokeError,
  type BridgeErrorPayload,
} from "./errors.js";

/** Documented narrow layout breakpoint (px). Packages use this constant. */
export const NARROW_BREAKPOINT_PX = 760;

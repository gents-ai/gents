/* How long the client waits and how often it asks; tests shorten them. */
export type AppTiming = {
  p2pAutoRestartCooldownMs: number;
  clientRestartMaxAttempts: number;
  clientRestartBackoffMs: number;
  activeSessionPollMs: number | null;
};

const DEFAULT_TIMING_CONFIG: AppTiming = {
  p2pAutoRestartCooldownMs: 20_000,
  clientRestartMaxAttempts: 10,
  clientRestartBackoffMs: 250,
  activeSessionPollMs: 1_500,
};

let timingConfigOverrides: Partial<AppTiming> | null = null;

export function timingConfig(): AppTiming {
  return {
    ...DEFAULT_TIMING_CONFIG,
    ...timingConfigOverrides,
  };
}

export function setTimingForTests(overrides: Partial<AppTiming> | null) {
  timingConfigOverrides = overrides;
}

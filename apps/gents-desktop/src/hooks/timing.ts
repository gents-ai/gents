export type AppTiming = {
  p2pAutoRestartCooldownMs: number;
  clientRestartMaxAttempts: number;
  clientRestartBackoffMs: number;
  /** the least time between full reads of an open session while store
      updates keep arriving for it and a live read can stand in; null leaves
      a live session to live reads until one asks for a full read */
  sessionReadGapMs: number | null;
};

const DEFAULT_TIMING_CONFIG: AppTiming = {
  p2pAutoRestartCooldownMs: 20_000,
  clientRestartMaxAttempts: 10,
  clientRestartBackoffMs: 250,
  sessionReadGapMs: 1_500,
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

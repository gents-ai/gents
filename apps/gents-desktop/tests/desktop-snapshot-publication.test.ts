import { describe, expect, it, vi } from "vitest";

import type {
  BackendSaveRequest,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createDesktopShellConfigActions } from "../src/hooks/desktopShellConfigActions";
import { createSnapshotPublicationOwner } from "../src/hooks/desktopSnapshotPublication";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function snapshot(label: string): DesktopClientSnapshot {
  return { label } as unknown as DesktopClientSnapshot;
}

describe("desktop snapshot publication", () => {
  it("publishes only the newest issued asynchronous observation", () => {
    const published: DesktopClientSnapshot[] = [];
    const owner = createSnapshotPublicationOwner((next) => published.push(next));
    const older = owner.begin();
    const newer = owner.begin();

    expect(newer.publish(snapshot("newer"))).toBe(true);
    expect(older.publish(snapshot("older"))).toBe(false);
    expect(published).toEqual([snapshot("newer")]);
  });

  it("orders crossed config-save results by invocation, not completion", async () => {
    const first = deferred<DesktopClientSnapshot>();
    const second = deferred<DesktopClientSnapshot>();
    const saveBackendConfig = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    let current: DesktopClientSnapshot | null = null;
    const owner = createSnapshotPublicationOwner((next) => {
      current = next;
    });
    const actions = createDesktopShellConfigActions({
      api: { saveBackendConfig } as unknown as DesktopApiAdapter,
      beginSnapshotPublication: owner.begin,
      setError: vi.fn(),
      setSavingBehaviorConfig: vi.fn(),
      setSavingConfig: vi.fn(),
      setSelectedAgentDid: vi.fn(),
      setSelectedBehaviorId: vi.fn(),
    });
    const older = actions.onSaveBackendConfig({} as BackendSaveRequest);
    const newer = actions.onSaveBackendConfig({} as BackendSaveRequest);
    second.resolve(snapshot("newer"));
    await newer;
    first.resolve(snapshot("older"));
    await older;

    expect(current).toEqual(snapshot("newer"));
  });
});

import { describe, expect, it } from "vitest";

import { projectStartupLoadingStatus } from "../src/lib/loadingStatus";

describe("startup loading projection", () => {
  it("reports only lifecycle-owned startup work", () => {
    expect(projectStartupLoadingStatus("checking-managed-server", true)).toMatchObject({
      currentLabel: "Starting your local node",
      managedServerState: "active",
      connectionState: "pending",
      clientState: "pending",
    });
    expect(projectStartupLoadingStatus("loading-configuration")).toMatchObject({
      currentLabel: "Reading saved connections",
      connectionState: "active",
      clientState: "pending",
    });
    expect(projectStartupLoadingStatus("managed-server-error", true)).toMatchObject({
      failed: true,
      managedServerState: "error",
      connectionState: "pending",
    });
    expect(projectStartupLoadingStatus("starting-client")).toMatchObject({
      currentLabel: "Starting the secure client",
      connectionState: "complete",
      clientState: "active",
    });
  });
});

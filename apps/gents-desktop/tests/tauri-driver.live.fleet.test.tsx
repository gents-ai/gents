import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";

describeLive("Tauri app live fleet add flow", () => {
  it("adds a live runner directly from its /status address", async () => {
    await withLiveDesktop(async ({ runner, driver, deployment: firstDeployment }) => {
      await driver.ready();
      logTurn(`fleet driver ready statusUrl=${runner.baseUrl}/status`);

      await driver.user.click(screen.getByRole("button", { name: "Add Agent" }));
      await driver.replaceInput("fleet-add-server-address", runner.baseUrl);
      await driver.user.click(screen.getByTestId("fleet-fetch-status"));

      await waitFor(
        async () => {
          const latest = await runner.fetchSnapshot();
          const deployment = latest.client?.deployments.find(
            (candidate) => candidate.agentDid === runner.agentDid,
          );
          expect(deployment).toBeDefined();
          expect(deployment?.addr).toBe(firstDeployment.addr);
          expect(deployment?.label).toBe(firstDeployment.label);
          expect(
            screen.queryByTestId("fleet-add-server-address"),
          ).not.toBeInTheDocument();
          const peerId = deployment?.peerId;
          expect(peerId).toBeTruthy();
          expect(screen.getByTestId(`fleet-row-${peerId}`)).toBeInTheDocument();
        },
        { timeout: 60_000 },
      );

      expect(screen.queryByText(/failed|error|not found/i)).not.toBeInTheDocument();
    });
  }, 300_000);

  it("keeps the add form usable after failed discovery and invalid manual input", async () => {
    await withLiveDesktop(async ({ runner, driver }) => {
      await driver.ready();
      const initialDeploymentCount =
        (await runner.fetchSnapshot()).client?.deployments.length ?? 0;

      await driver.user.click(screen.getByRole("button", { name: "Add Agent" }));
      await driver.replaceInput("fleet-add-server-address", "http://127.0.0.1:9");
      await driver.user.click(screen.getByTestId("fleet-fetch-status"));

      await waitFor(
        () => {
          expect(screen.getByTestId("error-banner")).toHaveTextContent(
            /error|connect|connection|refused|fetch failed/i,
          );
        },
        { timeout: 30_000 },
      );
      expect(driver.input("fleet-add-server-address")).toBeEnabled();
      expect(screen.getByTestId("fleet-fetch-status")).toBeEnabled();

      await driver.user.click(screen.getByText("Enter connection details manually"));
      await driver.replaceInput("fleet-add-label", "Invalid Peer");
      await driver.replaceInput("fleet-add-agent-did", "   ");
      await driver.replaceInput("fleet-add-addr", "iroh://invalid-peer");
      await driver.user.click(screen.getByTestId("fleet-add-submit"));

      await waitFor(() => {
        expect(screen.getByText(/Agent DID is required/i)).toBeInTheDocument();
      });
      expect(driver.input("fleet-add-label")).toHaveValue("Invalid Peer");
      expect(driver.input("fleet-add-agent-did")).toHaveValue("   ");
      expect(screen.getByTestId("fleet-add-submit")).toBeEnabled();
      expect((await runner.fetchSnapshot()).client?.deployments.length ?? 0).toBe(
        initialDeploymentCount,
      );
    });
  }, 180_000);
});

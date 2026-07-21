import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import { describeLive, logTurn } from "./tauri-driver-live/helpers";

describeLive("Tauri app live bridge runner sad paths", () => {
  it("surfaces a missing-model inference failure and returns the composer to ready", async () => {
    const baseModel =
      process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME ?? "missing-live-model";
    const missingModel = `${baseModel}__defra_missing_sad_path__`;
    await withLiveDesktop(
      async ({ runner, driver }) => {
        await driver.ready();
        await driver.openChat();
        logTurn(
          `sad path ready deployment=${runner.deploymentLabel} model=${missingModel}`,
        );

        await driver.typeComposer("Reply with one short sentence.");
        await driver.pressEnter();
        await waitFor(() => {
          expect(runner.sendResults).toHaveLength(1);
        });
        const submitted = expectLatestSendResult(runner, "missing-model turn");
        logTurn(
          `sad path submitted sessionId=${submitted.sessionId} requestId=${submitted.requestId}`,
        );

        const failedSession = await runner.waitForRequestCompletion(submitted);
        expect(failedSession.turnState).toBe("failed");
        expect(failedSession.latestResponse?.errorMessage).toBeTruthy();

        await waitFor(
          () => {
            expect(screen.getByTestId("response-error-card")).toHaveTextContent(
              /agent stream failed|model|404|not found/i,
            );
          },
          { timeout: 30_000 },
        );

        await driver.typeComposer("Can I type after the failure?");
        await waitFor(() => {
          expect(driver.sendButton()).toBeEnabled();
        });
      },
      { modelName: missingModel },
    );
  }, 240_000);

  it("surfaces an unreachable inference endpoint and returns the composer to ready", async () => {
    await withLiveDesktop(
      async ({ runner, driver }) => {
        await driver.ready();
        await driver.openChat();
        logTurn(
          `unreachable inference ready deployment=${runner.deploymentLabel} endpoint=http://127.0.0.1:9/v1`,
        );

        await driver.typeComposer("Reply with one short sentence.");
        await driver.pressEnter();
        await waitFor(() => {
          expect(runner.sendResults).toHaveLength(1);
        });
        const submitted = expectLatestSendResult(runner, "unreachable turn");
        logTurn(
          `unreachable inference submitted sessionId=${submitted.sessionId} requestId=${submitted.requestId}`,
        );

        const failedSession = await runner.waitForRequestCompletion(submitted);
        expect(failedSession.turnState).toBe("failed");
        expect(failedSession.latestResponse?.errorMessage).toMatch(
          /agent stream failed|connection|connect|refused|error sending request|transport/i,
        );

        await waitFor(
          () => {
            expect(screen.getByTestId("response-error-card")).toHaveTextContent(
              /agent stream failed|connection|connect|refused|error sending request|transport/i,
            );
          },
          { timeout: 30_000 },
        );

        await driver.typeComposer("Can I type after the unreachable backend failure?");
        await waitFor(() => {
          expect(driver.sendButton()).toBeEnabled();
        });
      },
      {
        inferenceUrl: "http://127.0.0.1:9/v1",
        modelName:
          process.env.DEFRA_AGENT_TAURI_LIVE_MODEL_NAME ??
          "defra-unreachable-live-model",
      },
    );
  }, 240_000);

  it("surfaces a bad MCP service probe without leaving config unusable", async () => {
    await withLiveDesktop(async ({ driver }) => {
      const suffix = Date.now().toString();

      await driver.ready();
      await driver.openConfig();
      await driver.openConfigSection("metaTools");
      await driver.user.click(screen.getByTestId("tool-service-new"));
      await driver.replaceInput("tool-service-id", `bad-mcp-${suffix}`);
      await driver.replaceInput("tool-service-display-name", "Bad MCP Probe");
      await driver.replaceInput("tool-service-hostname", "127.0.0.1");
      await driver.replaceInput("tool-service-mcp-port", "9");
      await driver.replaceInput("tool-service-mcp-path", "/mcp");

      await driver.user.click(screen.getByTestId("tool-service-test"));

      await waitFor(
        () => {
          expect(screen.getByTestId("tool-service-test-error")).toHaveTextContent(
            /error|connect|connection|refused|timed out|transport/i,
          );
        },
        { timeout: 30_000 },
      );
      expect(screen.getByTestId("tool-service-save")).toBeEnabled();

      await driver.replaceInput("tool-service-display-name", "Bad MCP Probe Edited");
      expect(screen.getByTestId("tool-service-display-name")).toHaveValue(
        "Bad MCP Probe Edited",
      );
    });
  }, 180_000);
});

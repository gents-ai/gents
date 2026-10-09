import { createServer, request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";

import { screen, waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import {
  expectLatestSendResult,
  liveRunnerOptionsFromEnv,
  withLiveDesktop,
} from "./tauri-driver-live/harness";
import {
  describeLive,
  logTurn,
  waitForDeploymentDocument,
} from "./tauri-driver-live/helpers";

async function withInferenceFault(
  fault: "missing-model" | "disconnect",
  run: (proxy: {
    inferenceUrl: string;
    arm: () => void;
    attempts: () => number;
    providerRejections: () => number;
  }) => Promise<void>,
) {
  const options = liveRunnerOptionsFromEnv();
  const upstream = new URL(options.inferenceUrl!);
  let armed = false;
  let attempts = 0;
  let providerRejections = 0;
  const pending = new Set<ReturnType<typeof httpRequest>>();
  const server = createServer(async (incoming, outgoing) => {
    try {
      const chunks: Buffer[] = [];
      for await (const chunk of incoming) chunks.push(Buffer.from(chunk));
      let body = Buffer.concat(chunks);
      const completion =
        incoming.method === "POST" &&
        /\/(chat\/completions|completions|responses)(?:\?|$)/.test(incoming.url ?? "");
      const inject = armed && completion;
      if (inject) {
        attempts += 1;
        if (fault === "disconnect") {
          incoming.socket.destroy();
          return;
        }
        const payload = JSON.parse(body.toString("utf8"));
        payload.model = `${options.modelName}__gents_missing_sad_path__`;
        body = Buffer.from(JSON.stringify(payload));
      }
      const suffix = (incoming.url ?? "/v1").replace(/^\/v1/, "");
      const target = new URL(`${upstream.href.replace(/\/$/, "")}${suffix}`);
      const headers = { ...incoming.headers, host: target.host };
      delete headers["transfer-encoding"];
      if (body.length || incoming.headers["content-length"]) {
        headers["content-length"] = String(body.length);
      }
      const request = (target.protocol === "https:" ? httpsRequest : httpRequest)(
        target,
        { method: incoming.method, headers },
        (response) => {
          if (response.statusCode === undefined) {
            outgoing.destroy();
            return;
          }
          if (inject && response.statusCode >= 400) providerRejections += 1;
          outgoing.writeHead(response.statusCode, response.headers);
          response.pipe(outgoing);
        },
      );
      pending.add(request);
      request.on("close", () => pending.delete(request));
      request.on("error", () => outgoing.destroy());
      outgoing.on("close", () => request.destroy());
      request.end(body);
    } catch {
      outgoing.destroy();
    }
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string")
    throw new Error("Proxy has no TCP address");
  try {
    await run({
      inferenceUrl: `http://127.0.0.1:${address.port}/v1`,
      arm: () => {
        expect(attempts).toBe(0);
        armed = true;
      },
      attempts: () => attempts,
      providerRejections: () => providerRejections,
    });
  } finally {
    for (const request of pending) request.destroy();
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
}

describeLive("Tauri app live bridge runner sad paths", () => {
  it("surfaces a missing-model inference failure and returns the composer to ready", async () => {
    await withInferenceFault("missing-model", async (proxy) => {
      await withLiveDesktop(
        async ({ runner, driver }) => {
          await driver.ready();
          await driver.openChat();
          logTurn(`sad path ready deployment=${runner.deploymentLabel}`);

          proxy.arm();
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
          expect(proxy.attempts()).toBeGreaterThan(0);
          expect(proxy.providerRejections()).toBeGreaterThan(0);
          expect(failedSession.turnState).toBe("failed");
          expect(failedSession.latestRequestOutcome?.failureReason).toBeTruthy();

          await waitFor(
            () => {
              expect(
                screen.getByText("The assistant could not finish this turn.")
                  .parentElement,
              ).toHaveTextContent(/agent stream failed|model|404|not found/i);
            },
            { timeout: 30_000 },
          );

          await driver.typeComposer("Can I type after the failure?");
          await waitFor(() => {
            expect(driver.sendButton()).toBeEnabled();
          });
        },
        { inferenceUrl: proxy.inferenceUrl },
      );
    });
  }, 240_000);

  it("surfaces an unreachable inference endpoint and returns the composer to ready", async () => {
    await withInferenceFault("disconnect", async (proxy) => {
      await withLiveDesktop(
        async ({ runner, driver }) => {
          await driver.ready();
          await driver.openChat();
          logTurn(
            `unreachable inference ready deployment=${runner.deploymentLabel} fault=completion-disconnect`,
          );

          proxy.arm();
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
          expect(proxy.attempts()).toBeGreaterThan(0);
          expect(failedSession.turnState).toBe("failed");
          expect(failedSession.latestRequestOutcome?.failureReason).toMatch(
            /agent stream failed|connection|connect|refused|error sending request|transport/i,
          );

          await waitFor(
            () => {
              expect(
                screen.getByText("The assistant could not finish this turn.")
                  .parentElement,
              ).toHaveTextContent(
                /agent stream failed|connection|connect|refused|error sending request|transport/i,
              );
            },
            { timeout: 30_000 },
          );

          await driver.typeComposer(
            "Can I type after the unreachable backend failure?",
          );
          await waitFor(() => {
            expect(driver.sendButton()).toBeEnabled();
          });
        },
        { inferenceUrl: proxy.inferenceUrl },
      );
    });
  }, 240_000);

  it("surfaces a bad MCP service probe without leaving config unusable", async () => {
    await withLiveDesktop(async ({ runner, driver }) => {
      const name = `Bad MCP Probe ${Date.now()}`;

      await driver.ready();
      await driver.openConfig();
      await driver.openConfigSection("tool-services");
      const existingServices = new Set(
        (
          await runner.fetchSnapshot()
        ).client?.deployments[0]?.toolServiceRegistries.map(
          (service) => service.service_id,
        ),
      );
      await driver.user.click(screen.getByRole("button", { name: "New remote tools" }));
      let serviceId = "";
      await waitForDeploymentDocument(runner, (deployment) => {
        const created = deployment.toolServiceRegistries.filter(
          (service) => !existingServices.has(service.service_id),
        );
        expect(created).toHaveLength(1);
        serviceId = created[0]!.service_id;
      });
      await driver.openConfigSection("tool-services");
      await driver.openConfigItem(serviceId);
      const replace = async (label: string, value: string) => {
        const input = await screen.findByLabelText(label);
        await driver.user.clear(input);
        await driver.user.type(input, value);
      };
      await replace("Display name", name);
      await replace("Hostname", "127.0.0.1");
      await replace("MCP port", "9");
      await replace("MCP path", "/mcp");
      await driver.user.click(screen.getByRole("button", { name: "Save" }));
      await waitForDeploymentDocument(runner, (deployment) => {
        const service = deployment.toolServiceRegistries.find(
          (candidate) => candidate.service_id === serviceId,
        );
        expect(service).toBeDefined();
        expect(service?.display_name).toBe(name);
        expect(service?.node_did).toBe(deployment.nodeDid);
        expect(service?.hostname).toBe("127.0.0.1");
        expect(service?.mcp_port).toBe(9);
        expect(service?.mcp_path).toBe("/mcp");
      });

      await driver.user.click(screen.getByRole("button", { name: "Test connection" }));
      await waitFor(
        () => {
          expect(
            screen.getByText(
              /Test failed:.*(?:connect|connection|refused|timed out|transport)/i,
            ),
          ).toBeVisible();
        },
        { timeout: 30_000 },
      );
      await replace("Display name", "Bad MCP Probe Edited");
      expect(screen.getByLabelText("Display name")).toHaveValue("Bad MCP Probe Edited");
      expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    });
  }, 180_000);
});

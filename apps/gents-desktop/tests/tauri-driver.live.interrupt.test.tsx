import { createServer, request as httpRequest } from "node:http";
import { request as httpsRequest } from "node:https";

import { screen, waitFor } from "@testing-library/react";
import { isTerminalTurnState } from "@source-inc/gents-desktop-client";
import { expect, it } from "vitest";

import {
  expectLatestSendResult,
  liveRunnerOptionsFromEnv,
  withLiveDesktop,
} from "./tauri-driver-live/harness";
import {
  describeLive,
  expectCompletedSession,
  FIRST_PROMPT,
  logTurn,
} from "./tauri-driver-live/helpers";

const FOLLOW_UP_PROMPT =
  "After the interrupt flow, answer with exactly one short sentence: ready-after-interrupt.";

async function withHeldTargetInference(
  run: (proxy: { inferenceUrl: string; attempts: () => number }) => Promise<void>,
) {
  const options = liveRunnerOptionsFromEnv();
  const upstream = new URL(options.agentTargetInferenceUrl ?? options.inferenceUrl!);
  let attempts = 0;
  const pending = new Set<ReturnType<typeof httpRequest>>();
  const server = createServer(async (incoming, outgoing) => {
    try {
      const chunks: Buffer[] = [];
      for await (const chunk of incoming) chunks.push(Buffer.from(chunk));
      const body = Buffer.concat(chunks);
      if (
        incoming.method === "POST" &&
        body.toString("utf8").includes("live-child-control") &&
        /\/(chat\/completions|completions|responses|messages)(?:\?|$)/.test(
          incoming.url ?? "",
        )
      ) {
        attempts += 1;
        return;
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
      attempts: () => attempts,
    });
  } finally {
    for (const request of pending) request.destroy();
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => {
      server.close((error) => (error ? reject(error) : resolve()));
    });
  }
}

describeLive("Tauri app live interrupt flow", () => {
  it("lets the user read and interrupt a running spawned session", async () => {
    await withHeldTargetInference(async (proxy) => {
      const options = liveRunnerOptionsFromEnv();
      await withLiveDesktop(
        async ({ runner, driver, deployment }) => {
          const target = deployment.agentTargets[0];
          expect(target, "live fixture did not expose an agent target").toBeDefined();
          await driver.ready();
          await driver.openChat();
          await driver.typeComposer(
            "Use the configured local agent target. Call agent_new with that agent and ask it to read workspace/AGENTS.md and return the phrase live-child-control with one short finding. Then reply with one sentence saying it has started.",
          );
          await driver.pressEnter();
          await waitFor(() => expect(runner.sendResults).toHaveLength(1));
          const parent = expectLatestSendResult(runner, "child control parent");

          const child = await waitFor(
            async () => {
              const fleet = await runner.adapter.fetchDesktopSnapshot();
              const parentSummary = fleet.client?.deployments
                .flatMap((node) => node.sessions)
                .find(
                  (candidate) =>
                    candidate.nodeDid === runner.nodeDid &&
                    candidate.sessionId === parent.sessionId,
                );
              expect(parentSummary?.requesterDid).toBeTruthy();
              const provenance = await runner.adapter.sessionProvenance({
                sessionId: parent.sessionId,
                nodeDid: runner.nodeDid,
                requesterDid: parentSummary!.requesterDid,
              });
              expect(provenance.started).toHaveLength(1);
              const link = provenance.started[0]!;
              expect(link.sessionId).not.toBe(parent.sessionId);
              const snapshot = await runner.adapter.fetchSessionSnapshot(
                link.sessionId,
                link.nodeDid,
              );
              expect(proxy.attempts()).toBeGreaterThan(0);
              expect(snapshot?.agentDid).toBe(target!.agent_did);
              expect(snapshot?.turnState).toBe("running");
              expect(snapshot?.latestRequestId).toBeTruthy();
              expect(
                snapshot?.timelineItems.some(
                  (item) =>
                    (item.kind === "userMessage" || item.kind === "automatedInput") &&
                    /live-child-control/i.test(item.content ?? ""),
                ),
                "the user should be able to read the spawned session's request",
              ).toBe(true);
              return { ...link, requestId: snapshot!.latestRequestId! };
            },
            { timeout: 120_000 },
          );

          const result = await runner.adapter.interruptRequest({
            nodeDid: child.nodeDid,
            requestId: child.requestId,
            cause: "userCancelled",
          });
          expect(result.requestId).toBe(child.requestId);
          expect(result.accepted).toBe(true);
          expect(result.alreadyInterrupted).toBe(false);
          const interrupted = await runner.waitForRequestCompletion(child);
          expect(interrupted.sessionId).toBe(child.sessionId);
          expect(interrupted.latestRequestId).toBe(child.requestId);
          expect(interrupted.turnState).toBe("interrupted");
          expect(interrupted.latestRequestOutcome?.cancelCause?.cause).toBe(
            "userCancelled",
          );
          expect(interrupted.latestRequestOutcome?.cancelCause?.source).toBe(
            "requestInterrupt",
          );
        },
        {
          agentTargetInferenceUrl: proxy.inferenceUrl,
          agentTargetProvider: options.agentTargetProvider ?? options.provider,
        },
      );
    });
  }, 600_000);

  it.each(["immediate", "streaming"] as const)(
    "recovers the composer after %s interruption",
    async (phase) => {
      await withLiveDesktop(async ({ runner, driver }) => {
        await driver.ready();
        await driver.openChat();
        logTurn(`driver ready nodeDid=${runner.nodeDid}`);

        await driver.typeComposer(
          phase === "streaming"
            ? "Without using tools, write a detailed 2000-word explanation of how a document-driven agent runtime works. Keep writing until the explanation is complete."
            : FIRST_PROMPT,
        );
        await driver.pressEnter();
        await waitFor(() => {
          expect(runner.sendResults).toHaveLength(1);
        });
        const submitted = expectLatestSendResult(runner, "interrupt turn");
        logTurn(
          `turn submitted sessionId=${submitted.sessionId} requestId=${submitted.requestId}`,
        );

        await waitFor(
          () => {
            const btn = driver.cancelButton();
            expect(btn).toBeTruthy();
            expect(btn).toBeEnabled();
          },
          { timeout: 15_000 },
        );
        logTurn("cancel button enabled");

        let streamedContent = "";
        let streamedReasoning = "";
        if (phase === "streaming") {
          await waitFor(
            async () => {
              const session = await runner.adapter.fetchSessionSnapshot(
                submitted.sessionId,
                runner.nodeDid,
                submitted.requestId,
              );
              expect(session?.turnState).toBe("running");
              const response = session?.timelineItems.find(
                (item) => item.kind === "liveAssistant",
              );
              expect((response?.content ?? "").length).toBeGreaterThan(0);
              expect((response?.reasoning ?? "").length).toBeGreaterThan(0);
              streamedContent = response?.content ?? "";
              streamedReasoning = response?.reasoning ?? "";
            },
            { timeout: 90_000 },
          );
          logTurn("live provider output observed before interrupt");
        }

        await driver.clickCancel();
        logTurn("interrupt clicked");

        const finalSession = await runner.waitForRequestCompletion(submitted);
        const terminalObservedAt = performance.now();
        if (phase === "streaming") {
          const retained = await runner.fetchRetainedProviderReasoning(
            submitted.sessionId,
            submitted.requestId,
          );
          expect(retained.requestDocId.length).toBeGreaterThan(0);
          expect(retained.reasoningBySource.length).toBeGreaterThan(0);
          expect(
            retained.reasoningBySource.some((reasoning) =>
              reasoning.includes(streamedReasoning),
            ),
            JSON.stringify({ streamedReasoning, retained }),
          ).toBe(true);
        }
        const cancelCause = finalSession?.latestRequestOutcome?.cancelCause;
        if (cancelCause) {
          expect(finalSession.turnState).toBe("interrupted");
          expect(cancelCause.cause).toBe("userCancelled");
          expect(cancelCause.source).toBe("requestInterrupt");
          if (phase === "streaming") {
            const retained = finalSession.timelineItems.filter(
              (item) => item.kind === "assistantMessage",
            );
            expect(retained.length).toBeGreaterThan(0);
            expect(retained.map((item) => item.content ?? "").join("")).toContain(
              streamedContent,
            );
          }
          logTurn(`interrupt latched: cause=${cancelCause.cause}`);
          await waitFor(
            () => {
              expect(screen.getByTestId("stopped-notice")).toHaveTextContent(
                "You stopped this response.",
              );
            },
            { timeout: 30_000 },
          );
        } else {
          expect(isTerminalTurnState(finalSession.turnState)).toBe(true);
          expect(finalSession.turnState).not.toBe("interrupted");
          logTurn(
            "turn finished before interrupt could affect it — " +
              "bridge call succeeded without error (race outcome: turn completed first, no cancelCause latched)",
          );
        }

        await driver.typeComposer(FOLLOW_UP_PROMPT);
        try {
          await waitFor(
            () => {
              expect(
                driver.sendButton(),
                `follow-up send disabled: ${driver.composerPlaceholder()}`,
              ).toBeEnabled();
            },
            { timeout: 30_000 },
          );
        } catch (error) {
          const diagnostics = await runner.fetchRequestDiagnostics(
            submitted.sessionId,
            submitted.requestId,
          );
          throw new Error(
            `follow-up composer did not recover; terminal=${JSON.stringify(finalSession)} diagnostics=${JSON.stringify(diagnostics)}: ${String(error)}`,
          );
        }
        logTurn(
          `composer recovered after terminal observation in ${Math.round(performance.now() - terminalObservedAt)}ms`,
        );
        await driver.pressEnter();
        await waitFor(() => {
          expect(runner.sendResults).toHaveLength(2);
        });
        const followUp = expectLatestSendResult(runner, "interrupt follow-up");
        expect(followUp.sessionId).toBe(submitted.sessionId);
        expect(followUp.requestId).not.toBe(submitted.requestId);
        logTurn(
          `follow-up submitted sessionId=${followUp.sessionId} requestId=${followUp.requestId}`,
        );

        const followUpSession = await runner.waitForRequestCompletion(followUp);
        expectCompletedSession("interrupt follow-up", followUpSession);
        expect(followUpSession.latestRequestId).toBe(followUp.requestId);
        expect(followUpSession.pendingTurn).toBeNull();
        expect(
          followUpSession.timelineItems.some((item) => item.kind === "liveAssistant"),
        ).toBe(false);

        await waitFor(
          () => {
            expect(screen.getAllByText(FOLLOW_UP_PROMPT)).toHaveLength(1);
            expect(driver.cancelButton()).toBeNull();
          },
          { timeout: 30_000 },
        );

        await driver.typeComposer("Composer should be ready after follow-up.");
        await waitFor(() => {
          expect(driver.sendButton()).toBeEnabled();
        });
      });
    },
    600_000,
  );
});

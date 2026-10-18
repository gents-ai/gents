import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { waitFor } from "@testing-library/react";
import { expect, it } from "vitest";

import { expectLatestSendResult, withLiveDesktop } from "./tauri-driver-live/harness";
import {
  delay,
  describeLive,
  expectCompletedSession,
  logTurn,
} from "./tauri-driver-live/helpers";
import { isTerminalTurnState } from "./live-bridge-runner/observations";

const E2E_NOTE_RELATIVE = "workspace/e2e-acceptance-note.md";
const FIXTURE_DEFAULT_AGENT_DISPLAY_NAME = "Live Repo Audit Default";

const e2eAcceptancePrompt = (sentinel: string) =>
  `Engineering acceptance check. You MUST call the bash_unrestricted tool once to create the file ${E2E_NOTE_RELATIVE} containing exactly one line: ${sentinel}. The file must contain exactly ${sentinel} followed by a single newline and nothing else; use a shell printf or echo redirect, not any other file tool. Then call read_file on ${E2E_NOTE_RELATIVE} to confirm the write. Finally explain the write and read-back verification in twenty numbered paragraphs, with two complete sentences in each paragraph, including ${sentinel}. The long explanation is required to exercise live streaming before completion.`;

describeLive("Tauri app native e2e acceptance (live fixture runtime)", () => {
  it("writes and reads back a sentinel file through the fixture tool surface, observes real streaming, and survives a session reload", async () => {
    const sentinel = `e2e-acceptance-${Date.now()}`;
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      expect(deployment.nodeDid).toContain("did:key");
      expect(runner.toolRoot).not.toContain(process.env.HOME ?? "/Users/");
      logTurn(
        `isolated deployment=${runner.deploymentLabel} nodeDid=${runner.nodeDid} toolRoot=${runner.toolRoot}`,
      );

      const expectedInferenceUrl =
        process.env.GENTS_TAURI_LIVE_INFERENCE_URL ??
        process.env.GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT ??
        "http://workstation-1:8000/v1";
      const expectedModelName =
        process.env.GENTS_TAURI_LIVE_MODEL_NAME ??
        process.env.GENTS_DESKTOP_LIVE_BACKEND_MODEL ??
        "GLM-5.3-Flash-NVFP4";
      const backend = deployment.inferenceBackends.find(
        (candidate) => candidate.enabled !== false && candidate.endpoint,
      );
      expect(
        backend,
        `fixture runtime exposed no enabled inference backend: ${JSON.stringify(
          deployment.inferenceBackends,
        )}`,
      ).toBeDefined();
      expect(backend?.endpoint).toBe(expectedInferenceUrl);
      const defaultAgent = deployment.agents.find(
        (agent) => agent.agentId === deployment.node.defaultAgentId,
      );
      expect(
        defaultAgent,
        `fixture runtime exposed no default agent: ${JSON.stringify(
          deployment.agents,
        )}`,
      ).toBeDefined();
      expect(defaultAgent?.displayName).toBe(FIXTURE_DEFAULT_AGENT_DISPLAY_NAME);
      const profile = deployment.inferenceProfiles.find(
        (candidate) => candidate.profile_id === defaultAgent?.inferenceProfileId,
      );
      expect(
        profile,
        `fixture runtime exposed no inference profile for the default agent: ${JSON.stringify(
          deployment.inferenceProfiles,
        )}`,
      ).toBeDefined();
      expect(profile?.model_name).toBe(expectedModelName);
      logTurn(
        `fixture runtime endpoint=${backend?.endpoint} model=${profile?.model_name} agent=${defaultAgent?.agentId}`,
      );

      await driver.ready();
      await driver.openChat();

      await driver.typeComposer(e2eAcceptancePrompt(sentinel));
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = expectLatestSendResult(runner, "e2e acceptance turn");
      logTurn(
        `turn submitted sessionId=${submitted.sessionId} requestId=${submitted.requestId}`,
      );

      let streamingObservation: {
        contentChars: number;
        reasoningChars: number;
      } | null = null;
      const streamingTimeoutMs = 120_000;
      const streamingDeadline = Date.now() + streamingTimeoutMs;
      while (streamingObservation === null) {
        if (Date.now() > streamingDeadline) {
          throw new Error(
            `no nonterminal streaming observation within ${streamingTimeoutMs}ms for request ${submitted.requestId}`,
          );
        }
        const live = await runner.adapter.fetchSessionSnapshot(
          submitted.sessionId,
          runner.nodeDid,
          submitted.requestId,
        );
        if (live && isTerminalTurnState(live.turnState)) {
          throw new Error(
            `turn reached terminal state ${live.turnState} before any nonterminal streaming observation; timeline=${JSON.stringify(live.timelineItems)}`,
          );
        }
        const liveResponse = live?.timelineItems.find(
          (item) => item.kind === "liveAssistant",
        );
        const contentChars =
          liveResponse?.kind === "liveAssistant"
            ? (liveResponse.content ?? "").length
            : 0;
        const reasoningChars =
          liveResponse?.kind === "liveAssistant"
            ? (liveResponse.reasoning ?? "").length
            : 0;
        if (live?.turnState === "running" && contentChars + reasoningChars > 0) {
          streamingObservation = { contentChars, reasoningChars };
        } else {
          await delay(150);
        }
      }
      logTurn(
        `nonterminal streaming observed contentChars=${streamingObservation.contentChars} reasoningChars=${streamingObservation.reasoningChars}`,
      );

      const session = await runner.waitForRequestCompletion(submitted);
      if (session.turnState !== "completed") {
        const diagnostics = await runner.fetchRequestDiagnostics(
          submitted.sessionId,
          submitted.requestId,
        );
        throw new Error(
          `e2e acceptance turn failed diagnostics=${JSON.stringify(diagnostics)}`,
        );
      }
      expectCompletedSession("e2e acceptance turn", session);
      expect(session.sessionId).toBe(submitted.sessionId);
      expect(session.latestRequestId).toBe(submitted.requestId);

      const toolCalls = session.timelineItems.flatMap((item) =>
        item.kind === "toolGroup" ? item.tools : [],
      );
      const bashCall = toolCalls.find((tool) =>
        /bash_unrestricted/i.test(tool.toolName),
      );
      expect(
        bashCall,
        `expected a bash_unrestricted tool call; saw tool names=${JSON.stringify(
          toolCalls.map((tool) => tool.toolName),
        )}`,
      ).toBeDefined();
      const bashPresentation = bashCall?.presentation;
      expect(
        bashPresentation?.kind,
        `bash call did not project a command presentation: ${JSON.stringify(bashPresentation)}`,
      ).toBe("command");
      if (bashPresentation?.kind === "command") {
        expect(
          bashPresentation.failed,
          `bash write command failed: ${JSON.stringify(bashPresentation)}`,
        ).toBe(false);
        expect(bashPresentation.exitCode).toBe(0);
      }
      const readBack = toolCalls.find((tool) => /read_file/i.test(tool.toolName));
      expect(
        readBack,
        `expected a read_file read-back call; saw tool names=${JSON.stringify(
          toolCalls.map((tool) => tool.toolName),
        )}`,
      ).toBeDefined();
      expect(
        readBack?.presentation.kind,
        `read-back did not project a fileRead presentation: ${JSON.stringify(readBack)}`,
      ).toBe("fileRead");
      if (readBack?.presentation.kind === "fileRead") {
        expect(
          readBack.presentation.body,
          `read-back body must be the canonical read_file rendering of the single sentinel line: ${JSON.stringify(readBack.presentation)}`,
        ).toBe(`content:\nL1: ${sentinel}`);
      }

      const notePath = join(runner.toolRoot, E2E_NOTE_RELATIVE);
      expect(existsSync(notePath), `missing written file ${notePath}`).toBe(true);
      const noteContents = readFileSync(notePath, "utf8");
      expect(
        noteContents,
        `written file must contain exactly the sentinel line: ${JSON.stringify(noteContents)}`,
      ).toBe(`${sentinel}\n`);

      const assistantTexts = session.timelineItems.flatMap((item) =>
        item.kind === "assistantMessage" ? [item.content ?? ""] : [],
      );
      expect(
        assistantTexts.some((content) => content.includes(sentinel)),
        `timeline assistant messages missing sentinel: ${JSON.stringify(assistantTexts)}`,
      ).toBe(true);
      expect(session.pendingTurn ?? null).toBeNull();
      expect(session.timelineItems.some((item) => item.kind === "liveAssistant")).toBe(
        false,
      );

      const reloaded = await runner.adapter.fetchSessionSnapshot(
        submitted.sessionId,
        runner.nodeDid,
        null,
      );
      expect(reloaded, "session reload returned no snapshot").toBeDefined();
      expect(reloaded?.sessionId).toBe(submitted.sessionId);
      expect(reloaded?.turnState).toBe("completed");
      const reloadedAssistantTexts = (reloaded?.timelineItems ?? []).flatMap((item) =>
        item.kind === "assistantMessage" ? [item.content ?? ""] : [],
      );
      expect(
        reloadedAssistantTexts.some((content) => content.includes(sentinel)),
        `reloaded timeline assistant messages missing sentinel: ${JSON.stringify(reloadedAssistantTexts)}`,
      ).toBe(true);
      const reloadedToolNames = reloaded?.timelineItems.flatMap((item) =>
        item.kind === "toolGroup" ? item.tools.map((tool) => tool.toolName) : [],
      );
      expect(
        reloadedToolNames?.some((name) => /bash_unrestricted/i.test(name)),
        `reloaded session lost the tool call: ${JSON.stringify(reloadedToolNames)}`,
      ).toBe(true);
      expect(
        reloaded?.timelineItems.some((item) => item.kind === "liveAssistant"),
      ).toBe(false);

      await driver.openChat();
      expect(driver.composer()).toBeInTheDocument();
      logTurn(
        `session reload verified sessionId=${submitted.sessionId} durableMessageCount=${reloaded?.context.durableMessageCount}`,
      );
    });
  }, 600_000);
});

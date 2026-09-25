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

/* Native e2e acceptance against the live fixture runtime — NOT the managed
   backend product path. The real App is rendered against bridge_runner with
   the live fixture (isolated desktop/remote/agent homes under a tempdir); the
   configured live inference endpoint/model is asserted through the canonical
   generated DeploymentView types; a scripted acceptance prompt forces one
   harmless file write + read-back through the live tool surface; a real
   nonterminal streaming observation is required; and the session is reloaded
   afterwards to prove the assistant final text and tool calls are durable.
   The fixture ships only the generic "Live Repo Audit Default" behavior; The
   Engineer behavior is NOT exercised here. Real
   DMG install, first-run setup/start/config, and manual acceptance remain
   separate requirements.
   Requires GENTS_TAURI_LIVE=1 (set by tests/run-live-test.mjs) and real
   inference. Run with:
     npm run test:live:e2e-acceptance -- \
       --inference-url http://workstation-1:8000/v1 \
       --model-name GLM-5.3-Flash-NVFP4 */

const E2E_NOTE_RELATIVE = "workspace/e2e-acceptance-note.md";
const FIXTURE_DEFAULT_BEHAVIOR_DISPLAY_NAME = "Live Repo Audit Default";

const e2eAcceptancePrompt = (sentinel: string) =>
  `Engineering acceptance check. You MUST call the bash_unrestricted tool once to create the file ${E2E_NOTE_RELATIVE} containing exactly one line: ${sentinel}. The file must contain exactly ${sentinel} followed by a single newline and nothing else; use a shell printf or echo redirect, not any other file tool. Then call read_file on ${E2E_NOTE_RELATIVE} to confirm the write. Finally explain the write and read-back verification in twenty numbered paragraphs, with two complete sentences in each paragraph, including ${sentinel}. The long explanation is required to exercise live streaming before completion.`;

describeLive("Tauri app native e2e acceptance (live fixture runtime)", () => {
  it("writes and reads back a sentinel file through the fixture tool surface, observes real streaming, and survives a session reload", async () => {
    const sentinel = `e2e-acceptance-${Date.now()}`;
    await withLiveDesktop(async ({ runner, driver, deployment }) => {
      // (1) Isolated homes: the fixture runs the desktop client, the remote
      // node, and the agent home under its own tempdir, never the daily
      // user home. The runner tool root is inside that tempdir.
      expect(deployment.agentDid).toContain("did:key");
      expect(runner.toolRoot).not.toContain(process.env.HOME ?? "/Users/");
      logTurn(
        `isolated deployment=${runner.deploymentLabel} agentDid=${runner.agentDid} toolRoot=${runner.toolRoot}`,
      );

      // (2) Fixture runtime configuration, asserted through the canonical
      // generated DeploymentView types (no guessed fields). bridge_runner
      // here is the test fixture runtime, not the managed backend; the
      // real setup/start/config acceptance is separate.
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
      const defaultBehavior = deployment.behaviors.find(
        (behavior) =>
          behavior.behaviorId === deployment.agentPrincipal.defaultBehaviorId,
      );
      expect(
        defaultBehavior,
        `fixture runtime exposed no default behavior: ${JSON.stringify(
          deployment.behaviors,
        )}`,
      ).toBeDefined();
      // The fixture ships a generic repository-audit behavior, not The
      // Engineer; Engineer behavior acceptance is out of scope for this
      // fixture and stays uncovered.
      expect(defaultBehavior?.displayName).toBe(FIXTURE_DEFAULT_BEHAVIOR_DISPLAY_NAME);
      const profile = deployment.inferenceProfiles.find(
        (candidate) => candidate.profile_id === defaultBehavior?.inferenceProfileId,
      );
      expect(
        profile,
        `fixture runtime exposed no inference profile for the default behavior: ${JSON.stringify(
          deployment.inferenceProfiles,
        )}`,
      ).toBeDefined();
      expect(profile?.model_name).toBe(expectedModelName);
      logTurn(
        `fixture runtime endpoint=${backend?.endpoint} model=${profile?.model_name} behavior=${defaultBehavior?.behaviorId}`,
      );

      await driver.ready();
      await driver.openChat();

      // (3) Scripted acceptance prompt that forces a real, harmless file
      // operation through the live tool surface.
      await driver.typeComposer(e2eAcceptancePrompt(sentinel));
      await driver.pressEnter();
      await waitFor(() => {
        expect(runner.sendResults).toHaveLength(1);
      });
      const submitted = expectLatestSendResult(runner, "e2e acceptance turn");
      logTurn(
        `turn submitted sessionId=${submitted.sessionId} requestId=${submitted.requestId}`,
      );

      // (4) Real nonterminal streaming observation: poll the same adapter
      // path the UI uses and require the canonical liveAssistant item to carry
      // actual streamed model output while the turn is still non-terminal.
      // Reaching a terminal state with no streamed overlay observation is
      // an explicit failure, not a silent pass.
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
          runner.agentDid,
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

      // (5) Tool output surfaced in the session timeline: one bash command
      // call that succeeded, plus the read-back of the written file.
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
      // Assert the canonical presentation kind BEFORE checking the body so
      // a projection change fails loudly instead of silently skipping the
      // body assertion.
      expect(
        readBack?.presentation.kind,
        `read-back did not project a fileRead presentation: ${JSON.stringify(readBack)}`,
      ).toBe("fileRead");
      if (readBack?.presentation.kind === "fileRead") {
        // Canonical read_file rendering of a one-line file after envelope
        // stripping and trim_end: "content:\nL1: <line>".
        expect(
          readBack.presentation.body,
          `read-back body must be the canonical read_file rendering of the single sentinel line: ${JSON.stringify(readBack.presentation)}`,
        ).toBe(`content:\nL1: ${sentinel}`);
      }

      // The file must exist on disk under the isolated tool root with
      // exactly the sentinel line — the tool operation really happened.
      const notePath = join(runner.toolRoot, E2E_NOTE_RELATIVE);
      expect(existsSync(notePath), `missing written file ${notePath}`).toBe(true);
      const noteContents = readFileSync(notePath, "utf8");
      expect(
        noteContents,
        `written file must contain exactly the sentinel line: ${JSON.stringify(noteContents)}`,
      ).toBe(`${sentinel}\n`);

      // (6) The actual assistant final text (not only tool calls) must
      // carry the sentinel, and the turn must be terminal with nothing
      // pending and no leftover liveAssistant item.
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

      // (7) Session reload: re-fetch the session through the same adapter
      // path the UI uses on reload and re-drive the UI into the session.
      // The reloaded snapshot must keep the actual assistant final text and
      // the tool calls, with no liveAssistant item after the terminal turn.
      const reloaded = await runner.adapter.fetchSessionSnapshot(
        submitted.sessionId,
        runner.agentDid,
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

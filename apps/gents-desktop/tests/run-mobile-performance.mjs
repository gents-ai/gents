import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { arch, cpus, platform, release, totalmem } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright";

const TESTS_ROOT = dirname(fileURLToPath(import.meta.url));
const APP_ROOT = resolve(TESTS_ROOT, "..");
const REPOSITORY_ROOT = resolve(APP_ROOT, "..", "..");
const DEFAULT_RUNS = 5;
const DEFAULT_PORT = 1427;
const VIEWPORT = { width: 390, height: 844 };
const TYPING_NEXT_PAINT_BUDGET_MS = 50;
const TYPING_REACT_BUDGET_MS_PER_CHARACTER = 12;

const args = process.argv.slice(2);
const runs = integerArgument("--runs", DEFAULT_RUNS);
const port = integerArgument("--port", DEFAULT_PORT);
const enforceResponsiveBudgets = args.includes("--enforce-responsive-budgets");
const outputArgument = valueArgument("--output");
const outputRoot = resolve(
  APP_ROOT,
  outputArgument ??
    join(
      "test-results",
      "mobile-performance",
      new Date().toISOString().replaceAll(":", "-").replaceAll(".", "-"),
    ),
);
const baseUrl = `http://127.0.0.1:${port}`;

mkdirSync(outputRoot, { recursive: true });

const viteBin = resolve(
  dirname(import.meta.resolve("vite/package.json").replace("file://", "")),
  "bin",
  "vite.js",
);
const server = spawn(
  process.execPath,
  [
    viteBin,
    "--host",
    "127.0.0.1",
    "--port",
    String(port),
    "--strictPort",
    "--clearScreen",
    "false",
  ],
  { cwd: APP_ROOT, env: process.env, stdio: ["ignore", "pipe", "pipe"] },
);
let serverOutput = "";
server.stdout.on("data", (chunk) => {
  serverOutput += chunk.toString();
});
server.stderr.on("data", (chunk) => {
  serverOutput += chunk.toString();
});

let browser;
try {
  await waitForHttp(`${baseUrl}/tests/ui-harness/harness.html`);
  browser = await chromium.launch({ headless: true });
  const browserVersion = browser.version();
  const samples = [];
  for (let sampleIndex = 1; sampleIndex <= runs; sampleIndex += 1) {
    process.stdout.write(`Mobile performance sample ${sampleIndex}/${runs}\n`);
    samples.push(await runSample(browser, sampleIndex));
  }

  const environment = collectEnvironment(browserVersion);
  const workingTreeDirty = git(["status", "--porcelain"]).trim().length > 0;
  const artifact = {
    schemaVersion: 1,
    harness: "gents-mobile-interactions",
    generatedAt: new Date().toISOString(),
    baselineCommit: git(["rev-parse", "HEAD"]).trim(),
    baselineRef: git(["branch", "--show-current"]).trim(),
    workingTreeDirty,
    workingTreeFingerprint: workingTreeDirty ? workingTreeFingerprint() : null,
    measurementClass:
      "deterministic Chromium proxy at iPhone viewport; first browser-process sample is cold and excluded from warm distributions",
    comparableOnlyWhen: [
      "baselineCommit and clean/dirty working-tree fingerprint, fixture.id, browser engine, viewport, host class, and build profile match",
      "cold simulator samples are reported separately from this warm browser distribution",
    ],
    environment,
    fixture: samples[0]?.fixture ?? null,
    samples,
    coldBrowserProcessSample: samples[0] ?? null,
    distributions: summarizeDistributions(samples.slice(1)),
    structuralAssertions: evaluateStructuralAssertions(samples),
    responsiveBudgetsEnforced: enforceResponsiveBudgets,
    unsupportedInThisLane: [
      "iOS process resident-memory high-water mark",
      "device energy log and thermal state",
      "DefraDB merge counts beyond observer counters on a live native run",
      "real suspend/resume transport repair",
      "remote session hydration before issues #1142/#1143 land",
    ],
  };
  const jsonPath = join(outputRoot, "mobile-performance.json");
  const summaryPath = join(outputRoot, "mobile-performance.md");
  writeFileSync(jsonPath, `${JSON.stringify(artifact, null, 2)}\n`);
  writeFileSync(summaryPath, humanSummary(artifact));
  process.stdout.write(`Machine artifact: ${jsonPath}\n`);
  process.stdout.write(`Human summary: ${summaryPath}\n`);

  const failures = artifact.structuralAssertions.filter(
    (entry) =>
      !entry.passed && (entry.policy === "hard" || artifact.responsiveBudgetsEnforced),
  );
  if (failures.length > 0) {
    throw new Error(
      `gated mobile performance assertions failed: ${failures
        .map((entry) => entry.id)
        .join(", ")}`,
    );
  }
} finally {
  await browser?.close();
  server.kill("SIGTERM");
}

async function runSample(browserInstance, sampleIndex) {
  const context = await browserInstance.newContext({ viewport: VIEWPORT });
  const page = await context.newPage();
  await page.addInitScript(() => {
    const state = {
      longTasks: [],
      mutations: 0,
    };
    Object.defineProperty(window, "__GENTS_BROWSER_PERFORMANCE__", {
      value: state,
      configurable: false,
    });
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        state.longTasks.push({
          startTimeMs: entry.startTime,
          durationMs: entry.duration,
        });
      }
    }).observe({ type: "longtask", buffered: true });
    window.addEventListener("DOMContentLoaded", () => {
      new MutationObserver((records) => {
        state.mutations += records.length;
      }).observe(document.documentElement, {
        subtree: true,
        childList: true,
        characterData: true,
        attributes: true,
      });
    });
  });
  const cdp = await context.newCDPSession(page);
  await cdp.send("Performance.enable");

  const navigationStartedAt = process.hrtime.bigint();
  await page.goto(
    `${baseUrl}/tests/ui-harness/harness.html?scenario=mobile-performance`,
    { waitUntil: "domcontentloaded" },
  );
  await page.locator('[data-testid="app-shell"]').waitFor({ state: "visible" });
  const navigationElapsedMs = monotonicElapsedMs(navigationStartedAt);
  await settleRender(page);
  const fixture = await page.evaluate(
    () => window.__GENTS_MOBILE_PERFORMANCE__.fixture,
  );
  const scenarios = [];

  scenarios.push(
    await captureInitialScenario(
      page,
      cdp,
      "cold_launch_to_shell_proxy",
      navigationElapsedMs,
      {
        cold: sampleIndex === 1,
        boundary: "navigation start -> visible application shell",
      },
    ),
  );

  await page
    .locator('[data-testid^="session-session-"]')
    .first()
    .waitFor({ state: "visible" });
  await settleRender(page);
  scenarios.push(
    await captureInitialScenario(
      page,
      cdp,
      "paired_launch_to_session_index",
      monotonicElapsedMs(navigationStartedAt),
      {
        cold: sampleIndex === 1,
        boundary: "navigation start -> first visible local session row",
      },
    ),
  );

  scenarios.push(
    await measureScenario(page, cdp, "open_cached_short_session", async () => {
      await page.locator('[data-testid="session-session-intro"]').click();
      await page
        .locator('[data-testid="transcript-panel"]')
        .getByText("I am your desktop UI test agent", { exact: false })
        .waitFor();
    }),
  );

  await openMobileNavigation(page);
  scenarios.push(
    await measureScenario(page, cdp, "open_large_local_transcript_tip", async () => {
      await page.locator('[data-testid="session-session-large"]').click();
      await page.getByText("stream-start", { exact: false }).last().waitFor();
    }),
  );

  const retainedRow = await page
    .getByText("User fixture row 560", { exact: false })
    .elementHandle();
  scenarios.push(
    await measureScenario(
      page,
      cdp,
      "page_older_transcript_rows",
      async () => {
        await page.locator('[data-testid="transcript-load-older"]').click();
        await page.getByText("User fixture row 520", { exact: false }).waitFor();
      },
      async () => ({
        retainedRowStillMounted: await retainedRow.evaluate((node) => node.isConnected),
      }),
    ),
  );

  const typingStartingRows = await page
    .locator(
      '[data-testid="transcript-panel"] [data-slot="assistant-message"], [data-testid="transcript-panel"] [data-slot="user-message"], [data-testid="transcript-panel"] [data-slot="tool-steps"]',
    )
    .count();
  scenarios.push(
    await measureScenario(
      page,
      cdp,
      "grow_long_transcript_to_typing_size",
      async () => {
        if (typingStartingRows % fixture.transcriptPageSize !== 0) {
          throw new Error(
            `typing transcript starts with a partial page: ${typingStartingRows} rows`,
          );
        }
        const startingPages = typingStartingRows / fixture.transcriptPageSize;
        const additionalPages = fixture.typingLoadedPages - startingPages;
        if (additionalPages < 0) {
          throw new Error(
            `typing transcript already exceeds the target: ${startingPages} pages`,
          );
        }
        for (let index = 0; index < additionalPages; index += 1) {
          const expectedRows =
            typingStartingRows + (index + 1) * fixture.transcriptPageSize;
          await page.locator('[data-testid="transcript-load-older"]').click();
          await page.waitForFunction(
            (count) =>
              document.querySelectorAll(
                '[data-testid="transcript-panel"] [data-slot="assistant-message"], [data-testid="transcript-panel"] [data-slot="user-message"], [data-testid="transcript-panel"] [data-slot="tool-steps"]',
              ).length === count,
            expectedRows,
          );
        }
      },
      async () => ({
        startingRows: typingStartingRows,
        loadedPages: fixture.typingLoadedPages,
        expectedLoadedRows: fixture.typingLoadedPages * fixture.transcriptPageSize,
      }),
    ),
  );

  scenarios.push(
    await measureScenario(page, cdp, "sustained_streamed_response", async () => {
      const count = fixture.streamUpdateCount;
      for (let index = 0; index < count; index += 1) {
        const sequence = await page.evaluate(() =>
          window.__GENTS_MOBILE_PERFORMANCE__.streamUpdate(),
        );
        await page
          .getByText(`stream-chunk-${sequence}`, { exact: false })
          .last()
          .waitFor();
      }
    }),
  );

  scenarios.push(
    await measureScenario(page, cdp, "update_coalescing_burst", async () => {
      const lastSequence = await page.evaluate(() => {
        return window.__GENTS_MOBILE_PERFORMANCE__.streamBurst(25);
      });
      await page
        .getByText(`stream-chunk-${lastSequence}`, { exact: false })
        .last()
        .waitFor();
    }),
  );

  const typingText = Array.from({ length: fixture.typingBurstCharacters }, (_, index) =>
    String.fromCharCode(97 + (index % 26)),
  ).join("");
  scenarios.push(
    await measureScenario(
      page,
      cdp,
      "typing_burst_active_turn_no_live_deltas_long_transcript",
      () => typeBurstToNextPaint(page, typingText),
      () => typingPaintEvidence(page, typingText.length, fixture.typingLoadedPages),
    ),
  );
  await page.getByRole("textbox", { name: "Message" }).fill("");
  await settleRender(page);

  scenarios.push(
    await measureScenario(
      page,
      cdp,
      "typing_burst_concurrent_stream_deltas_long_transcript",
      () => typeBurstWithConcurrentStreamDeltas(page, typingText),
      async () => ({
        ...(await typingPaintEvidence(
          page,
          typingText.length,
          fixture.typingLoadedPages,
        )),
        ...(await page.evaluate(() => window.__GENTS_CONCURRENT_STREAM_EVIDENCE__)),
      }),
    ),
  );
  await page.getByRole("textbox", { name: "Message" }).fill("");
  await settleRender(page);

  await page.evaluate(() => {
    window.__GENTS_MOBILE_PERFORMANCE__.finishStreaming();
  });
  await page.getByRole("button", { name: "Stop" }).waitFor({ state: "detached" });
  await settleRender(page);
  scenarios.push(
    await measureScenario(
      page,
      cdp,
      "typing_burst_idle_long_transcript",
      () => typeBurstToNextPaint(page, typingText),
      () => typingPaintEvidence(page, typingText.length, fixture.typingLoadedPages),
    ),
  );
  await page.getByRole("textbox", { name: "Message" }).fill("");
  await settleRender(page);

  const navigationHeapSamples = [];
  scenarios.push(
    await measureScenario(
      page,
      cdp,
      "repeated_navigation_memory",
      async () => {
        for (let index = 0; index < fixture.repeatedNavigationCount; index += 1) {
          await openMobileNavigation(page);
          await page.locator('[data-testid="session-session-intro"]').click();
          await page
            .locator('[data-testid="transcript-panel"]')
            .getByText("I am your desktop UI test agent", { exact: false })
            .waitFor();
          await openMobileNavigation(page);
          await page.locator('[data-testid="session-session-large"]').click();
          await page.getByText("stream-start", { exact: false }).last().waitFor();
          navigationHeapSamples.push((await browserMetrics(page, cdp)).jsHeapUsedBytes);
        }
        await cdp.send("HeapProfiler.collectGarbage");
      },
      async () => ({
        navigationHeapSamples,
        navigationHeapHighWaterBytes: Math.max(...navigationHeapSamples),
        navigationHeapGrowthBytes:
          navigationHeapSamples.at(-1) - navigationHeapSamples[0],
      }),
    ),
  );

  await context.close();
  return { sampleIndex, fixture, scenarios };
}

async function captureInitialScenario(page, cdp, id, elapsedMs, annotations) {
  const harness = await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__.snapshot(),
  );
  const browser = await browserMetrics(page, cdp);
  return {
    id,
    elapsedMs,
    bridge: bridgeSummary(harness.bridgeCalls),
    render: renderSummary(harness.commits),
    browser,
    dom: await domSnapshot(page),
    updateEvents: harness.updateEvents,
    annotations,
  };
}

async function measureScenario(page, cdp, id, action, extra = async () => ({})) {
  await page.evaluate(() => {
    window.__GENTS_MOBILE_PERFORMANCE__.reset();
    window.__GENTS_BROWSER_PERFORMANCE__.longTasks = [];
    window.__GENTS_BROWSER_PERFORMANCE__.mutations = 0;
  });
  const before = await browserMetrics(page, cdp);
  const startedAt = process.hrtime.bigint();
  await action();
  await settleRender(page);
  const elapsedMs = monotonicElapsedMs(startedAt);
  const after = await browserMetrics(page, cdp);
  const harness = await page.evaluate(() =>
    window.__GENTS_MOBILE_PERFORMANCE__.snapshot(),
  );
  const observed = await page.evaluate(() => window.__GENTS_BROWSER_PERFORMANCE__);
  return {
    id,
    elapsedMs,
    bridge: bridgeSummary(harness.bridgeCalls),
    render: renderSummary(harness.commits),
    browser: {
      taskDurationMs: Math.max(0, after.taskDurationMs - before.taskDurationMs),
      jsHeapUsedBytesBefore: before.jsHeapUsedBytes,
      jsHeapUsedBytesAfter: after.jsHeapUsedBytes,
      jsHeapGrowthBytes: after.jsHeapUsedBytes - before.jsHeapUsedBytes,
      longTaskCount: observed.longTasks.length,
      longTaskTotalMs: sum(observed.longTasks.map((entry) => entry.durationMs)),
      mutationRecordCount: observed.mutations,
    },
    dom: await domSnapshot(page),
    updateEvents: harness.updateEvents,
    ...(await extra()),
  };
}

async function typeBurstToNextPaint(page, text) {
  await prepareTypingPaintMeasurement(page);
  const input = page.getByRole("textbox", { name: "Message" });
  await input.focus();
  await input.pressSequentially(text, { delay: 1 });
  await page.waitForFunction(
    (expectedCharacters) =>
      (window.__GENTS_TYPING_PAINT_SAMPLES__?.length ?? 0) === expectedCharacters,
    text.length,
  );
}

async function prepareTypingPaintMeasurement(page) {
  await page.evaluate(() => {
    const samples = [];
    Object.defineProperty(window, "__GENTS_TYPING_PAINT_SAMPLES__", {
      value: samples,
      configurable: true,
    });
    const input = document.querySelector('textarea[aria-label="Message"]');
    if (!(input instanceof HTMLTextAreaElement)) {
      throw new Error("mobile performance composer is unavailable");
    }
    if (window.__GENTS_TYPING_INPUT_HANDLER__) {
      document.removeEventListener("input", window.__GENTS_TYPING_INPUT_HANDLER__, {
        capture: true,
      });
    }
    const recordNextPaint = (event) => {
      const eventAt = event.timeStamp;
      requestAnimationFrame(() => {
        setTimeout(() => samples.push(performance.now() - eventAt), 0);
      });
    };
    window.__GENTS_TYPING_INPUT_HANDLER__ = recordNextPaint;
    document.addEventListener("input", recordNextPaint, { capture: true });
  });
}

async function typeBurstWithConcurrentStreamDeltas(page, text) {
  await prepareTypingPaintMeasurement(page);
  await page.evaluate((expectedCharacters) => {
    window.__GENTS_CONCURRENT_STREAM_EVIDENCE__ = {
      streamUpdateCount: 0,
      firstStreamUpdateAtMs: null,
      lastStreamUpdateAtMs: null,
      typingStartedAtMs: null,
      typingFinishedAtMs: null,
      liveDeltaReadsDuringTyping: 0,
      lastSequence: null,
    };
    const evidence = window.__GENTS_CONCURRENT_STREAM_EVIDENCE__;
    let inputCount = 0;
    const observeTypingBoundary = (event) => {
      inputCount += 1;
      if (inputCount === 1) {
        evidence.typingStartedAtMs = event.timeStamp;
      }
      if (
        inputCount % 3 === 0 &&
        evidence.streamUpdateCount <
          window.__GENTS_MOBILE_PERFORMANCE__.fixture.streamUpdateCount
      ) {
        const updatedAt = performance.now();
        evidence.firstStreamUpdateAtMs ??= updatedAt;
        evidence.lastStreamUpdateAtMs = updatedAt;
        evidence.lastSequence = window.__GENTS_MOBILE_PERFORMANCE__.streamUpdate();
        evidence.streamUpdateCount += 1;
      }
      if (inputCount === expectedCharacters) {
        evidence.typingFinishedAtMs = event.timeStamp;
        evidence.liveDeltaReadsDuringTyping = window.__GENTS_MOBILE_PERFORMANCE__
          .snapshot()
          .bridgeCalls.filter(
            (call) => call.command === "fetchSessionLiveDelta",
          ).length;
        document.removeEventListener("input", observeTypingBoundary, {
          capture: true,
        });
      }
    };
    document.addEventListener("input", observeTypingBoundary, { capture: true });
  }, text.length);
  const input = page.getByRole("textbox", { name: "Message" });
  await input.focus();
  await input.pressSequentially(text, { delay: 2 });
  const lastSequence = await page.evaluate(
    () => window.__GENTS_CONCURRENT_STREAM_EVIDENCE__.lastSequence,
  );
  await page
    .getByText(`stream-chunk-${lastSequence}`, { exact: false })
    .last()
    .waitFor();
  await page.waitForFunction(
    (expectedCharacters) =>
      (window.__GENTS_TYPING_PAINT_SAMPLES__?.length ?? 0) === expectedCharacters,
    text.length,
  );
}

async function typingPaintEvidence(page, expectedCharacters, loadedPages) {
  const samples = await page.evaluate(
    () => window.__GENTS_TYPING_PAINT_SAMPLES__ ?? [],
  );
  return {
    typedCharacters: samples.length,
    expectedCharacters,
    loadedPages,
    eventToNextPaintMs: distribution(samples),
  };
}

async function browserMetrics(page, cdp) {
  const response = await cdp.send("Performance.getMetrics");
  const metrics = Object.fromEntries(
    response.metrics.map((entry) => [entry.name, entry.value]),
  );
  return {
    taskDurationMs: (metrics.TaskDuration ?? 0) * 1000,
    jsHeapUsedBytes:
      metrics.JSHeapUsedSize ??
      (await page.evaluate(() => performance.memory?.usedJSHeapSize ?? 0)),
  };
}

async function domSnapshot(page) {
  return page.evaluate(() => ({
    sessionRows: document.querySelectorAll('[data-testid^="session-session-"]').length,
    transcriptMessageCards: document.querySelectorAll(
      '[data-testid="transcript-panel"] [data-slot="assistant-message"], [data-testid="transcript-panel"] [data-slot="user-message"]',
    ).length,
    transcriptTurnBlocks: document.querySelectorAll(
      '[data-testid="transcript-panel"] [data-slot="assistant-message"], [data-testid="transcript-panel"] [data-slot="user-message"], [data-testid="transcript-panel"] [data-slot="tool-steps"]',
    ).length,
    serializedBodyBytes: new TextEncoder().encode(document.body.innerHTML).byteLength,
  }));
}

function bridgeSummary(calls) {
  const byCommand = {};
  for (const call of calls) {
    const entry = (byCommand[call.command] ??= {
      count: 0,
      requestBytes: 0,
      responseBytes: 0,
      durationMs: 0,
      maxResponseBytes: 0,
    });
    entry.count += 1;
    entry.requestBytes += call.requestBytes;
    entry.responseBytes += call.responseBytes;
    entry.durationMs += call.durationMs;
    entry.maxResponseBytes = Math.max(entry.maxResponseBytes, call.responseBytes);
  }
  return {
    callCount: calls.length,
    requestBytes: sum(calls.map((call) => call.requestBytes)),
    responseBytes: sum(calls.map((call) => call.responseBytes)),
    byCommand,
  };
}

function renderSummary(commits) {
  const durations = commits.map((commit) => commit.actualDurationMs);
  return {
    commitCount: commits.length,
    totalCommitDurationMs: sum(durations),
    maxCommitDurationMs: durations.length ? Math.max(...durations) : 0,
    p95CommitDurationMs: percentile(durations, 0.95),
  };
}

function summarizeDistributions(samples) {
  const scenarioIds = samples[0]?.scenarios.map((scenario) => scenario.id) ?? [];
  return scenarioIds.map((id) => {
    const scenarios = samples.map((sample) =>
      sample.scenarios.find((scenario) => scenario.id === id),
    );
    const elapsed = scenarios.map((scenario) => scenario.elapsedMs);
    const responseBytes = scenarios.map((scenario) => scenario.bridge.responseBytes);
    const bridgeCalls = scenarios.map((scenario) => scenario.bridge.callCount);
    const commitDuration = scenarios.map(
      (scenario) => scenario.render.totalCommitDurationMs,
    );
    const commitCounts = scenarios.map((scenario) => scenario.render.commitCount);
    const taskDuration = scenarios.map((scenario) => scenario.browser.taskDurationMs);
    const heapGrowth = scenarios.map((scenario) => scenario.browser.jsHeapGrowthBytes);
    const longTasks = scenarios.map((scenario) => scenario.browser.longTaskCount);
    const transcriptRows = scenarios.map(
      (scenario) => scenario.dom.transcriptTurnBlocks,
    );
    const eventToNextPaintP95 = scenarios
      .map((scenario) => scenario.eventToNextPaintMs?.p95)
      .filter(Number.isFinite);
    const navigationHeapHighWater = scenarios
      .map((scenario) => scenario.navigationHeapHighWaterBytes)
      .filter(Number.isFinite);
    return {
      id,
      sampleCount: elapsed.length,
      elapsedMs: distribution(elapsed),
      bridgeResponseBytes: distribution(responseBytes),
      bridgeCallCount: distribution(bridgeCalls),
      renderCommitDurationMs: distribution(commitDuration),
      renderCommitCount: distribution(commitCounts),
      taskDurationMs: distribution(taskDuration),
      jsHeapGrowthBytes: distribution(heapGrowth),
      longTaskCount: distribution(longTasks),
      transcriptTurnBlocks: distribution(transcriptRows),
      ...(eventToNextPaintP95.length
        ? { eventToNextPaintP95Ms: distribution(eventToNextPaintP95) }
        : {}),
      ...(navigationHeapHighWater.length
        ? { navigationHeapHighWaterBytes: distribution(navigationHeapHighWater) }
        : {}),
    };
  });
}

function evaluateStructuralAssertions(samples) {
  const all = (id) =>
    samples.map((sample) => sample.scenarios.find((scenario) => scenario.id === id));
  const tip = all("open_large_local_transcript_tip");
  const pageOlder = all("page_older_transcript_rows");
  const sustained = all("sustained_streamed_response");
  const burst = all("update_coalescing_burst");
  const typingActiveTurn = all(
    "typing_burst_active_turn_no_live_deltas_long_transcript",
  );
  const typingConcurrent = all("typing_burst_concurrent_stream_deltas_long_transcript");
  const typingIdle = all("typing_burst_idle_long_transcript");
  const typingWithoutLiveDeltas = [...typingActiveTurn, ...typingIdle];
  const typing = [...typingWithoutLiveDeltas, ...typingConcurrent];
  return [
    {
      id: "large_tip_mounts_one_page",
      policy: "hard",
      limit: 40,
      observedMax: Math.max(
        ...tip.map((scenario) => scenario.dom.transcriptTurnBlocks),
      ),
      passed: tip.every((scenario) => scenario.dom.transcriptTurnBlocks <= 40),
    },
    {
      id: "one_page_prepends_at_most_one_page",
      policy: "hard",
      limit: 80,
      observedMax: Math.max(
        ...pageOlder.map((scenario) => scenario.dom.transcriptTurnBlocks),
      ),
      passed: pageOlder.every(
        (scenario) =>
          scenario.dom.transcriptTurnBlocks <= 80 && scenario.retainedRowStillMounted,
      ),
    },
    {
      id: "typing_bursts_mount_five_pages",
      policy: "hard",
      limit:
        samples[0].fixture.typingLoadedPages * samples[0].fixture.transcriptPageSize,
      observedMax: Math.max(
        ...typing.map((scenario) => scenario.dom.transcriptTurnBlocks),
      ),
      passed: typing.every(
        (scenario) =>
          scenario.loadedPages === samples[0].fixture.typingLoadedPages &&
          scenario.dom.transcriptTurnBlocks ===
            samples[0].fixture.typingLoadedPages *
              samples[0].fixture.transcriptPageSize,
      ),
    },
    {
      id: "typing_bursts_capture_every_character",
      policy: "hard",
      limit: samples[0].fixture.typingBurstCharacters,
      observedMax: Math.max(...typing.map((scenario) => scenario.typedCharacters)),
      passed: typing.every(
        (scenario) =>
          scenario.typedCharacters === samples[0].fixture.typingBurstCharacters,
      ),
    },
    {
      id: "active_turn_without_deltas_and_idle_typing_have_no_bridge_calls",
      policy: "hard",
      limit: 0,
      observedMax: Math.max(
        ...typingWithoutLiveDeltas.map((scenario) => scenario.bridge.callCount),
      ),
      passed: typingWithoutLiveDeltas.every(
        (scenario) => scenario.bridge.callCount === 0,
      ),
    },
    {
      id: "concurrent_typing_observes_live_deltas_without_snapshots_or_submit",
      policy: "hard",
      limit: samples[0].fixture.streamUpdateCount,
      observedMax: Math.max(
        ...typingConcurrent.map(
          (scenario) => scenario.bridge.byCommand.fetchSessionLiveDelta?.count ?? 0,
        ),
      ),
      passed: typingConcurrent.every((scenario) => {
        const liveDeltaReads =
          scenario.bridge.byCommand.fetchSessionLiveDelta?.count ?? 0;
        return (
          scenario.streamUpdateCount === samples[0].fixture.streamUpdateCount &&
          scenario.firstStreamUpdateAtMs >= scenario.typingStartedAtMs &&
          scenario.firstStreamUpdateAtMs < scenario.typingFinishedAtMs &&
          scenario.lastStreamUpdateAtMs <= scenario.typingFinishedAtMs &&
          scenario.liveDeltaReadsDuringTyping > 0 &&
          scenario.liveDeltaReadsDuringTyping <= liveDeltaReads &&
          liveDeltaReads > 0 &&
          liveDeltaReads <= samples[0].fixture.streamUpdateCount &&
          scenario.bridge.responseBytes <= 64 * 1024 &&
          (scenario.bridge.byCommand.fetchSessionLiveDelta?.maxResponseBytes ?? 0) <=
            2 * 1024 &&
          (scenario.bridge.byCommand.fetchDesktopSnapshot?.count ?? 0) === 0 &&
          (scenario.bridge.byCommand.fetchSessionSnapshot?.count ?? 0) === 0 &&
          (scenario.bridge.byCommand.sendChatMessage?.count ?? 0) === 0 &&
          scenario.bridge.callCount === liveDeltaReads
        );
      }),
    },
    {
      id: "typing_event_to_next_paint_p95",
      policy: "responsive-budget",
      limit: TYPING_NEXT_PAINT_BUDGET_MS,
      observedMax: Math.max(
        ...typing.map((scenario) => scenario.eventToNextPaintMs.p95),
      ),
      passed: typing.every(
        (scenario) => scenario.eventToNextPaintMs.p95 <= TYPING_NEXT_PAINT_BUDGET_MS,
      ),
    },
    {
      id: "typing_react_work_per_character",
      policy: "responsive-budget",
      limit: TYPING_REACT_BUDGET_MS_PER_CHARACTER,
      observedMax: Math.max(
        ...typing.map(
          (scenario) =>
            scenario.render.totalCommitDurationMs / scenario.typedCharacters,
        ),
      ),
      passed: typing.every(
        (scenario) =>
          scenario.render.totalCommitDurationMs / scenario.typedCharacters <=
          TYPING_REACT_BUDGET_MS_PER_CHARACTER,
      ),
    },
    {
      id: "active_turn_burst_uses_one_live_delta_and_no_snapshots",
      policy: "hard",
      limit: 1,
      observedMax: Math.max(
        ...burst.map(
          (scenario) => scenario.bridge.byCommand.fetchSessionLiveDelta?.count ?? 0,
        ),
      ),
      passed: burst.every(
        (scenario) =>
          (scenario.bridge.byCommand.fetchDesktopSnapshot?.count ?? 0) === 0 &&
          (scenario.bridge.byCommand.fetchSessionSnapshot?.count ?? 0) === 0 &&
          (scenario.bridge.byCommand.fetchSessionLiveDelta?.count ?? 0) <= 1,
      ),
    },
    {
      id: "sustained_stream_avoids_full_session_projection",
      policy: "hard",
      limit: 0,
      observedMax: Math.max(
        ...sustained.map(
          (scenario) => scenario.bridge.byCommand.fetchSessionSnapshot?.count ?? 0,
        ),
      ),
      passed: sustained.every(
        (scenario) =>
          (scenario.bridge.byCommand.fetchDesktopSnapshot?.count ?? 0) === 0 &&
          (scenario.bridge.byCommand.fetchSessionSnapshot?.count ?? 0) === 0,
      ),
    },
    {
      id: "fifty_stream_deltas_stay_below_64_kib",
      policy: "hard",
      limit: 64 * 1024,
      observedMax: Math.max(
        ...sustained.map((scenario) => scenario.bridge.responseBytes),
      ),
      passed: sustained.every(
        (scenario) =>
          scenario.bridge.responseBytes <= 64 * 1024 &&
          (scenario.bridge.byCommand.fetchSessionLiveDelta?.count ?? 0) <= 50 &&
          (scenario.bridge.byCommand.fetchSessionLiveDelta?.maxResponseBytes ?? 0) <=
            2 * 1024,
      ),
    },
    {
      id: "session_page_payload_is_bounded",
      policy: "hard",
      limit: 16 * 1024,
      observedMax: Math.max(
        ...[...tip, ...pageOlder, ...burst].map(
          (scenario) =>
            scenario.bridge.byCommand.fetchSessionSnapshot?.maxResponseBytes ?? 0,
        ),
      ),
      passed: [...tip, ...pageOlder, ...burst].every(
        (scenario) =>
          (scenario.bridge.byCommand.fetchSessionSnapshot?.maxResponseBytes ?? 0) <=
          16 * 1024,
      ),
    },
  ];
}

function humanSummary(artifact) {
  const rows = artifact.distributions.map(
    (entry) =>
      `| ${entry.id} | ${entry.sampleCount} | ${format(entry.elapsedMs.median)} | ${format(entry.elapsedMs.p95)} | ${entry.eventToNextPaintP95Ms ? format(entry.eventToNextPaintP95Ms.median) : "—"} | ${formatBytes(entry.bridgeResponseBytes.median)} | ${format(entry.renderCommitDurationMs.median)} |`,
  );
  const assertions = artifact.structuralAssertions.map((entry) => {
    const gated = entry.policy === "hard" || artifact.responsiveBudgetsEnforced;
    return `- ${entry.passed ? "PASS" : "FAIL"} (${gated ? "gated" : "report only"}) \`${entry.id}\`: max ${entry.observedMax}, limit ${entry.limit}`;
  });
  return [
    "# Mobile performance evidence",
    "",
    `Baseline: \`${artifact.baselineCommit}\` (\`${artifact.baselineRef}\`)`,
    `Working tree: ${artifact.workingTreeDirty ? `dirty; SHA-256 \`${artifact.workingTreeFingerprint}\`` : "clean"}`,
    `Environment: ${artifact.environment.host.platform} ${artifact.environment.host.release}, ${artifact.environment.host.arch}, Chromium ${artifact.environment.browser.version}, ${VIEWPORT.width}x${VIEWPORT.height}`,
    `Fixture: \`${artifact.fixture.id}\`; ${artifact.fixture.sessionIndexCount} session cards; ${artifact.fixture.largeSessionTimelineItems} large-session timeline items.`,
    `Measurement class: ${artifact.measurementClass}.`,
    `Cold browser-process shell proxy (n=1, not a trend): ${format(artifact.coldBrowserProcessSample.scenarios[0].elapsedMs)} ms.`,
    "",
    "| Scenario | n | median ms | p95 ms | median input→paint p95 ms | median bridge response | median React commit ms |",
    "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ...rows,
    "",
    "## Benchmark assertions",
    "",
    `Responsive timing budgets: ${artifact.responsiveBudgetsEnforced ? "gated" : "report only; pass --enforce-responsive-budgets to gate them"}.`,
    "",
    ...assertions,
    "",
    "Wall-clock values are evidence only. They are not CI failure thresholds.",
    "",
  ].join("\n");
}

function collectEnvironment(browserVersion) {
  return {
    browser: { engine: "Chromium", version: browserVersion, viewport: VIEWPORT },
    host: {
      platform: platform(),
      release: release(),
      arch: arch(),
      logicalCpuCount: cpus().length,
      memoryBytes: totalmem(),
    },
    node: process.version,
    buildProfile: "Vite development transform; headless browser",
    temperatureState: "not captured",
  };
}

async function openMobileNavigation(page) {
  await page
    .getByTestId("session-screen")
    .getByRole("link", { name: "Sessions", exact: true })
    .first()
    .click();
  await page
    .locator('[data-testid="session-session-large"]')
    .waitFor({ state: "visible" });
}

async function settleRender(page) {
  await page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      ),
  );
}

async function waitForHttp(url) {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (server.exitCode != null) {
      throw new Error(`Vite exited before becoming ready:\n${serverOutput}`);
    }
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // The observable readiness condition is the HTTP response; retry until its deadline.
    }
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
  }
  throw new Error(`Vite did not become ready at ${url}:\n${serverOutput}`);
}

function monotonicElapsedMs(startedAt) {
  return Number(process.hrtime.bigint() - startedAt) / 1_000_000;
}

function distribution(values) {
  return {
    min: values.length ? Math.min(...values) : 0,
    median: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    max: values.length ? Math.max(...values) : 0,
  };
}

function percentile(values, fraction) {
  if (values.length === 0) return 0;
  const sorted = [...values].sort((left, right) => left - right);
  const index = (sorted.length - 1) * fraction;
  const lower = Math.floor(index);
  const upper = Math.ceil(index);
  if (lower === upper) return sorted[lower];
  return sorted[lower] + (sorted[upper] - sorted[lower]) * (index - lower);
}

function sum(values) {
  return values.reduce((total, value) => total + value, 0);
}

function format(value) {
  return Number(value).toFixed(1);
}

function formatBytes(value) {
  return value >= 1024 ? `${(value / 1024).toFixed(1)} KiB` : `${Math.round(value)} B`;
}

function valueArgument(name) {
  const equals = args.find((argument) => argument.startsWith(`${name}=`));
  if (equals) return equals.slice(name.length + 1);
  const index = args.indexOf(name);
  return index >= 0 ? args[index + 1] : null;
}

function integerArgument(name, fallback) {
  const value = valueArgument(name);
  if (value == null) return fallback;
  const parsed = Number.parseInt(value, 10);
  if (!Number.isInteger(parsed) || parsed < 1) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

function git(commandArgs) {
  return execFileSync("git", commandArgs, { cwd: REPOSITORY_ROOT, encoding: "utf8" });
}

function workingTreeFingerprint() {
  const hash = createHash("sha256");
  hash.update(git(["diff", "--binary", "HEAD", "--", "."]));
  const untracked = git(["ls-files", "--others", "--exclude-standard", "-z"])
    .split("\0")
    .filter(Boolean)
    .sort();
  for (const path of untracked) {
    hash.update(`\0${path}\0`);
    hash.update(readFileSync(resolve(REPOSITORY_ROOT, path)));
  }
  return hash.digest("hex");
}

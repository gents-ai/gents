export function waitForExitWithTimeout(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve({ kind: "exit", code: child.exitCode });
  }
  if (timeoutMs === null) {
    return waitForExit(child).then((code) => ({ kind: "exit", code }));
  }
  return new Promise((resolveExit) => {
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) {
        return;
      }
      settled = true;
      resolveExit({ kind: "timeout" });
    }, timeoutMs);
    child.once("exit", (code) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      resolveExit({ kind: "exit", code });
    });
  });
}

export async function runWithWatchdogRetry({
  startAttempt,
  timeoutMs,
  maxWatchdogRetries = 1,
  stopTimedOutChild = (child) => stopProcess(child),
  onRetry = () => {},
}) {
  for (let attempt = 1; attempt <= maxWatchdogRetries + 1; attempt += 1) {
    const child = await startAttempt(attempt);
    const result = await waitForExitWithTimeout(child, timeoutMs);
    if (result.kind === "exit") {
      return { ...result, attempts: attempt };
    }

    await stopTimedOutChild(child);
    if (attempt > maxWatchdogRetries) {
      return { kind: "timeout", attempts: attempt };
    }
    await onRetry({ attempt, nextAttempt: attempt + 1 });
  }

  throw new Error("unreachable Bombadil watchdog retry state");
}

export async function stopProcess(
  child,
  {
    killProcessGroup = false,
    graceMs = 2_000,
    killDrainMs = 2_000,
    killByPid = process.kill.bind(process),
  } = {},
) {
  if (!child) {
    return;
  }

  const alreadyExited = child.exitCode !== null || child.signalCode !== null;
  if (alreadyExited && !killProcessGroup) {
    return;
  }

  const exitPromise = alreadyExited ? Promise.resolve() : waitForExit(child);
  sendSignal(child, "SIGTERM", killProcessGroup, killByPid);
  await Promise.race([exitPromise, delay(graceMs)]);

  let sentFinalKill = false;
  if (killProcessGroup) {
    // Bombadil launches Chrome descendants. The CLI may exit on SIGTERM while
    // a wedged browser remains in its process group, so always fence the group
    // with SIGKILL after the grace period (ESRCH simply means it is clean).
    sendSignal(child, "SIGKILL", true, killByPid);
    sentFinalKill = true;
  } else if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGKILL");
    sentFinalKill = true;
  }

  if (sentFinalKill) {
    const [leaderDrain, groupDrained] = await Promise.all([
      waitForExitWithTimeout(child, killDrainMs),
      killProcessGroup
        ? waitForProcessGroupDrain(child.pid, killDrainMs, killByPid)
        : Promise.resolve(true),
    ]);
    if (leaderDrain.kind === "timeout" || !groupDrained) {
      throw new Error(
        `process group ${child.pid ?? "unknown"} did not drain within ${killDrainMs}ms after SIGKILL`,
      );
    }
  }
}

async function waitForProcessGroupDrain(pid, timeoutMs, killByPid) {
  if (!Number.isInteger(pid) || pid <= 0) {
    return true;
  }
  const deadline = Date.now() + timeoutMs;
  while (true) {
    try {
      killByPid(-pid, 0);
    } catch (error) {
      if (error?.code === "ESRCH") {
        return true;
      }
      if (error?.code !== "EPERM") {
        throw error;
      }
    }
    const remaining = deadline - Date.now();
    if (remaining <= 0) {
      return false;
    }
    await delay(Math.min(25, remaining));
  }
}

function sendSignal(child, signal, killProcessGroup, killByPid) {
  if (killProcessGroup && Number.isInteger(child.pid) && child.pid > 0) {
    try {
      killByPid(-child.pid, signal);
      return;
    } catch (error) {
      if (error?.code === "ESRCH") {
        if (child.exitCode === null && child.signalCode === null) {
          child.kill(signal);
        }
        return;
      }
      throw error;
    }
  }

  if (child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
  }
}

function waitForExit(child) {
  return new Promise((resolveExit) => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolveExit(child.exitCode);
      return;
    }
    child.once("exit", (code) => resolveExit(code));
  });
}

function delay(ms) {
  return new Promise((resolveDelay) => setTimeout(resolveDelay, ms));
}

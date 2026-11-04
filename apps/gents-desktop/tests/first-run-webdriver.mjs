// First run of the installed Gents desktop app, driven over W3C WebDriver.
// Zero dependencies: Node's fetch talks to WebKitWebDriver (Linux), which
// launches the shipped binary. Each step leaves a screenshot and the visible
// text; a failed wait leaves the DOM controls it could see.
import { mkdir, readFile, writeFile } from "node:fs/promises";

const env = process.env;
const base = env.WEBDRIVER_URL ?? "http://127.0.0.1:4444";
const out = env.FRL_OUT ?? "out";
const binary = env.GENTS_DESKTOP_BINARY ?? "/usr/bin/gents-desktop-tauri";
const inferenceUrl = env.FRL_INFERENCE_URL;
const wantedModel = env.FRL_MODEL ?? null;
const stopAfter = env.FRL_STOP_AFTER ?? null;
const sentinel = `first-run-${Date.now().toString(36)}`;
if (!inferenceUrl) throw new Error("FRL_INFERENCE_URL is required");
await mkdir(out, { recursive: true });

async function wd(method, path, body, timeoutMs = 60_000) {
  const res = await fetch(`${base}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const json = await res.json();
  if (!res.ok)
    throw new Error(`${method} ${path}: ${JSON.stringify(json.value).slice(0, 400)}`);
  return json.value;
}

const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";
// A binary that dies before its webview connects leaves the session request
// hanging; the driver log then holds the app's own launch error.
const { sessionId } = await wd(
  "POST",
  "/session",
  {
    capabilities: { alwaysMatch: { "webkitgtk:browserOptions": { binary, args: [] } } },
  },
  120_000,
).catch(async (error) => {
  const log = env.FRL_WEBDRIVER_LOG
    ? (await readFile(env.FRL_WEBDRIVER_LOG, "utf8").catch(() => ""))
        .trim()
        .split("\n")
        .slice(-5)
        .join("\n")
    : "";
  const failure = `app did not open a WebDriver session: ${error.message}${log ? `\n${log}` : ""}`;
  await writeFile(
    `${out}/report.json`,
    JSON.stringify({ steps: [{ name: "launch", ok: false }], failure }, null, 2),
  );
  console.error(failure);
  process.exit(1);
});
const s = (path) => `/session/${sessionId}${path}`;
const exec = (script, ...args) => wd("POST", s("/execute/sync"), { script, args });
const findAll = async (css) =>
  (await wd("POST", s("/elements"), { using: "css selector", value: css })).map(
    (e) => e[ELEMENT],
  );
const click = (el) => wd("POST", s(`/element/${el}/click`), {});
const textOf = (el) => wd("GET", s(`/element/${el}/text`));
async function type(el, value) {
  await wd("POST", s(`/element/${el}/clear`), {});
  await wd("POST", s(`/element/${el}/value`), { text: value });
}
const bodyText = () => exec("return document.body ? document.body.innerText : ''");
const controls = () =>
  exec(`return [...document.querySelectorAll('button,input,select,textarea,[role=radio],[role=option],[data-testid]')]
    .filter((e) => e.getClientRects().length)
    .map((e) => [e.tagName.toLowerCase(), e.getAttribute('data-testid'), e.getAttribute('aria-label'),
      e.id, e.getAttribute('role'), (e.innerText || e.value || '').trim().slice(0, 60)].join(' | '))`);

let shot = 0;
const report = { sentinel, inferenceUrl, binary, steps: [] };
async function capture(name) {
  shot += 1;
  const tag = `${String(shot).padStart(2, "0")}-${name}`;
  await writeFile(
    `${out}/${tag}.png`,
    Buffer.from(await wd("GET", s("/screenshot")), "base64"),
  );
  await writeFile(`${out}/${tag}.txt`, await bodyText());
  return tag;
}

async function waitFor(label, probe, timeoutMs = 60_000) {
  const until = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value) return value;
    if (Date.now() > until) {
      const tag = await capture(`stuck-${label}`);
      await writeFile(`${out}/${tag}.controls.txt`, (await controls()).join("\n"));
      throw new Error(`timed out waiting for ${label}; see ${tag}`);
    }
    await new Promise((r) => setTimeout(r, 500));
  }
}
const first = async (css) => (await findAll(css))[0] ?? null;
const textIncludes = (needle) => async () => (await bodyText()).includes(needle);

async function step(name, fn) {
  const started = Date.now();
  const entry = { name, ok: false };
  report.steps.push(entry);
  try {
    entry.detail = (await fn()) ?? null;
    entry.ok = true;
  } finally {
    entry.ms = Date.now() - started;
    entry.screenshot = await capture(name).catch(() => null);
    await writeFile(`${out}/report.json`, JSON.stringify(report, null, 2));
  }
  if (stopAfter === name) {
    await writeFile(`${out}/controls-${name}.txt`, (await controls()).join("\n"));
    throw new Error(`stopped after ${name} (FRL_STOP_AFTER)`);
  }
}

let failure = null;
try {
  await step("welcome", async () => {
    await waitFor("welcome", textIncludes("Let’s get set up"));
    // The tool root mounts only after home-directory discovery resolves.
    const filled = (id) => () =>
      exec("return document.getElementById(arguments[0])?.value || null", id);
    return {
      nodeName: await waitFor("node name", filled("setup-node-name")),
      toolRoot: await waitFor("tool root", filled("managed-tool-root")),
      ceiling: await exec(
        "return document.querySelector('[aria-labelledby=managed-tool-ceiling]')?.innerText",
      ),
    };
  });

  await step("start-node", async () => {
    await click(await first("[data-testid=setup-next]"));
    return waitFor(
      "provider choice",
      async () =>
        (await findAll("[data-testid^=setup-provider-]")).length ? true : null,
      240_000,
    ).then(async () =>
      exec(`return [...document.querySelectorAll('[data-testid^=setup-provider-]')]
        .map((e) => e.getAttribute('data-testid').replace('setup-provider-', ''))`),
    );
  });

  await step("connect-provider", async () => {
    const ids =
      await exec(`return [...document.querySelectorAll('[data-testid^=setup-provider-]')]
      .map((e) => e.getAttribute('data-testid'))`);
    const wanted = `setup-provider-${env.FRL_PROVIDER ?? "local"}`;
    const compatible = ids.find((id) => id === wanted);
    if (!compatible) throw new Error(`no ${wanted} among ${ids.join(", ")}`);
    await click(await first(`[data-testid="${compatible}"]`));
    const endpoint = await waitFor("endpoint field", () =>
      exec(`for (const input of document.querySelectorAll('input:not([type=hidden])')) {
          let node = input.parentElement;
          for (let depth = 0; node && depth < 4; depth += 1, node = node.parentElement) {
            if ((node.innerText || '').trim().split('\\n')[0].trim() === 'Endpoint') return input.id || null;
          }
        }
        return null;`),
    );
    await type(await first(`[id="${endpoint}"]`), inferenceUrl);
    return { provider: compatible };
  });

  await step("choose-model", async () => {
    const connect = await waitFor("connect button", async () => {
      for (const el of await findAll("button")) {
        if (
          /^(Connect|Discover models|Check connection)/i.test((await textOf(el)).trim())
        )
          return el;
      }
      return null;
    });
    await click(connect);
    await waitFor(
      "advertised models",
      async () =>
        (
          await findAll(
            "[aria-label='Advertised models'] [role=option], [aria-label='Advertised models'] button, [aria-label='Advertised models'] li",
          )
        ).length > 0,
      90_000,
    );
    const models = await exec(
      `return [...document.querySelectorAll("[aria-label='Advertised models'] [role=option], [aria-label='Advertised models'] button, [aria-label='Advertised models'] li")].map((e) => e.innerText.trim())`,
    );
    const options = await findAll(
      "[aria-label='Advertised models'] [role=option], [aria-label='Advertised models'] button, [aria-label='Advertised models'] li",
    );
    const index = wantedModel ? models.findIndex((m) => m.includes(wantedModel)) : 0;
    if (index < 0)
      throw new Error(`model ${wantedModel} not advertised: ${models.join(", ")}`);
    await click(options[index]);
    return { models, chose: models[index] };
  });

  await step("save-inference", async () => {
    await click(
      await waitFor("save button", () => first("[data-testid=setup-save-inference]")),
    );
    await waitFor(
      "chat composer",
      () => first("[aria-label=Message], [role=textbox][aria-label=Message]"),
      180_000,
    );
  });

  await step("first-message", async () => {
    const composer = await first("[aria-label=Message]");
    await click(composer);
    await wd("POST", s(`/element/${composer}/value`), {
      text: `Reply with exactly this token and nothing else: ${sentinel}`,
    });
    const send = await waitFor("send button", () => first("button[aria-label=Send]"));
    await click(send);
    // The prompt line carries other words; only the reply is the bare token.
    await waitFor(
      "assistant reply",
      async () =>
        (await bodyText()).split("\n").some((line) => line.trim() === sentinel),
      240_000,
    );
  });
} catch (error) {
  failure = String(error?.stack ?? error);
  report.failure = failure;
  await writeFile(`${out}/report.json`, JSON.stringify(report, null, 2));
} finally {
  await wd("DELETE", s("")).catch(() => {});
}
for (const step of report.steps)
  console.log(
    `${step.ok ? "PASS" : "FAIL"} ${step.name} ${step.ms}ms ${step.detail ? JSON.stringify(step.detail).slice(0, 300) : ""}`,
  );
if (failure) {
  console.error(failure);
  process.exit(1);
}

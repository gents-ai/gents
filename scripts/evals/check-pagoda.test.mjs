import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { execFile } from "node:child_process";
import test from "node:test";

const run = promisify(execFile);
const script = new URL("./check-pagoda.mjs", import.meta.url).pathname;
async function checkPage(html) {
  const root = await mkdtemp(path.join(tmpdir(), "gents-browser-contract-"));
  await writeFile(path.join(root, "index.html"), html);
  return run(process.execPath, [script, root, path.join(root, "evidence")]);
}

test("uncontrolled randomness remains inconclusive", async () => {
  await assert.rejects(
    checkPage(`<!doctype html><title>Pagoda</title>
    <button onclick="document.body.style.color='red'">Toggle night</button>
    <script>document.body.style.background = '#' + crypto.getRandomValues(new Uint32Array(1))[0].toString(16).padStart(8, '0').slice(0,6);</script>`),
    /matched-time control renders differ/,
  );
});

test("an overlay cannot satisfy the interaction check", async () => {
  await assert.rejects(
    checkPage(`<!doctype html><title>Pagoda</title>
    <button onclick="document.body.style.background='black'">Toggle night</button>
    <div style="position:fixed;inset:0;z-index:100"></div>`),
    /intercepts pointer events|Timeout/,
  );
});

for (const animated of [false, true]) {
  for (const changes of [true, false]) {
    test(`browser check ${changes ? "accepts" : "rejects"} (animated=${animated}, toggleWorks=${changes})`, async () => {
      const execution = checkPage(
        `<!doctype html><title>Pagoda</title>
      <h1>Pagoda</h1><button onclick="${changes ? "document.body.style.background='black'" : "void 0"}">Toggle night</button>
      ${
        animated
          ? `<canvas id="scene" width="300" height="100"></canvas><script>
        const ctx = scene.getContext('2d');
        function frame(t) { ctx.fillStyle = 'rgb(' + Math.floor(t / 10) % 255 + ',50,100)';
          ctx.fillRect(0, 0, 300, 100); requestAnimationFrame(frame); }
        requestAnimationFrame(frame);
      </script>`
          : ""
      }`,
      );
      if (changes) await execution;
      else
        await assert.rejects(execution, /toggle must produce a visible change/);
    });
  }
}

for (const [name, controls, expectedError] of [
  [
    "accepts a descriptive accessible-name suffix",
    '<button aria-label="Toggle night: change lighting" onclick="document.body.style.background=\'black\'">Toggle night</button>',
    null,
  ],
  [
    "accepts whitespace in a descriptively labelled control",
    '<button aria-label="Toggle night: change lighting" onclick="document.body.style.background=\'black\'">\n  <span>Toggle</span>\n  <span>night</span>\n</button>',
    null,
  ],
  [
    "rejects an accessible name that hides the visible label",
    '<button aria-label="Night mode">Toggle night</button>',
    /one accessible Toggle night button is required/,
  ],
  [
    "rejects a different visible label with a prefix-matching name",
    "<button>Toggle night mode</button>",
    /one accessible Toggle night button is required/,
  ],
  [
    "rejects a missing control",
    "<span>Toggle night</span>",
    /one accessible Toggle night button is required/,
  ],
  [
    "rejects ambiguous exact and extended labels",
    '<button>Toggle night</button><button aria-label="Toggle night: change lighting">Toggle night</button>',
    /one accessible Toggle night button is required/,
  ],
  [
    "still rejects a nonfunctional descriptively labelled control",
    '<button aria-label="Toggle night: change lighting">Toggle night</button>',
    /toggle must produce a visible change/,
  ],
]) {
  test(`browser label contract ${name}`, async () => {
    const execution = checkPage(
      `<!doctype html><title>Pagoda</title><h1>Pagoda</h1>${controls}`,
    );
    if (expectedError) await assert.rejects(execution, expectedError);
    else await execution;
  });
}

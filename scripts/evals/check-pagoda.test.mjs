import assert from "node:assert/strict";
import { mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { execFile } from "node:child_process";
import test from "node:test";

const run = promisify(execFile);
const script = new URL("./check-pagoda.mjs", import.meta.url).pathname;
for (const animated of [false, true]) {
  for (const changes of [true, false]) {
    test(`browser check ${animated ? "is inconclusive" : changes ? "accepts" : "rejects"} (animated=${animated}, toggleWorks=${changes})`, async () => {
      const root = await mkdtemp(
        path.join(tmpdir(), "gents-browser-contract-"),
      );
      await writeFile(
        path.join(root, "index.html"),
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
      const execution = run(process.execPath, [
        script,
        root,
        path.join(root, "evidence"),
      ]);
      if (animated)
        await assert.rejects(execution, /visible-change inconclusive/);
      else if (changes) await execution;
      else
        await assert.rejects(execution, /toggle must produce a visible change/);
    });
  }
}

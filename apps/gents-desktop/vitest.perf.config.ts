import { defineConfig } from "vitest/config";
import base from "./vitest.config";

/* timings, measured one file at a time so nothing else shares the machine's
   time with them; apart from the unit run, whose tests run in parallel */
export default defineConfig({
  ...base,
  test: {
    ...base.test,
    include: ["tests/perf/**/*.perf.ts"],
    fileParallelism: false,
  },
});

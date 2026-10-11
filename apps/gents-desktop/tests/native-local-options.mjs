export function parseNativeLocalOptions(argv, env = {}) {
  const allowed = new Set(["--skip-build", "--file-store-keys", "--help"]);
  for (const arg of argv) {
    if (!allowed.has(arg)) throw new Error(`Unsupported native E2E option: ${arg}`);
  }
  const custody = env.GENTS_E2E_FILE_STORE_KEYS;
  if (custody !== undefined && custody !== "0" && custody !== "1") {
    throw new Error("GENTS_E2E_FILE_STORE_KEYS must be 0 or 1");
  }
  return {
    skipBuild: argv.includes("--skip-build"),
    fileStoreKeys: argv.includes("--file-store-keys") || custody === "1",
    help: argv.includes("--help"),
  };
}

export const nativeLocalHelp = `Usage: npm run test:ui:native:local -- [--skip-build] [--file-store-keys]

--skip-build       Use fresh native-e2e app and CLI binaries.
--file-store-keys  Keep encrypted-store keys in this run's isolated homes,
                   avoiding macOS Keychain approval. DID signing and ACP remain enabled.
                   Production builds and homes are unchanged.

GENTS_E2E_FILE_STORE_KEYS=1 also selects file custody; 0 keeps normal Keychain custody.
`;

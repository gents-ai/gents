import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

const BRIDGE = "gents-desktop-bridge";
const DOMAIN = "fixture-domain";

function bridgeCmd(name: string) {
  return `plugin:${BRIDGE}|${name}`;
}
function domainCmd(name: string) {
  return `plugin:${DOMAIN}|${name}`;
}

/**
 * Thin raw-client UI for phase 4. Packages land in later phases; this host only
 * proves co-residence of bridge + domain plugins under separate homes.
 */
export function App() {
  const [log, setLog] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const push = useCallback((line: string) => {
    setLog((prev) => [line, ...prev].slice(0, 40));
  }, []);

  const run = useCallback(
    async (label: string, fn: () => Promise<unknown>) => {
      setBusy(true);
      setError(null);
      try {
        const result = await fn();
        push(`${label}: ${JSON.stringify(result).slice(0, 400)}`);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setError(message);
        push(`${label} ERROR: ${message}`);
      } finally {
        setBusy(false);
      }
    },
    [push],
  );

  return (
    <main>
      <h1 data-testid="fixture-title">Gents Fixture Host</h1>
      <p>
        Downstream shell: own bundle id, <code>AppDataDir</code> home, paired-remote
        bootstrap (no runtime-admin), co-resident domain plugin.
      </p>

      <div className="panel">
        <strong>Gents bridge</strong>
        <div>
          <button
            disabled={busy}
            data-testid="bridge-start"
            onClick={() =>
              run("client_start", () => invoke(bridgeCmd("desktop_client_start")))
            }
          >
            Start client
          </button>
          <button
            disabled={busy}
            data-testid="bridge-contract"
            onClick={() =>
              run("bridge_contract", () => invoke(bridgeCmd("desktop_bridge_contract")))
            }
          >
            Contract
          </button>
          <button
            disabled={busy}
            data-testid="bridge-snapshot"
            onClick={() =>
              run("client_snapshot", () => invoke(bridgeCmd("desktop_client_snapshot")))
            }
          >
            Snapshot
          </button>
        </div>
      </div>

      <div className="panel">
        <strong>Domain plugin</strong>
        <div>
          <button
            disabled={busy}
            data-testid="domain-home"
            onClick={() =>
              run("domain_home", () => invoke(domainCmd("domain_home_path")))
            }
          >
            Domain home path
          </button>
          <button
            disabled={busy}
            data-testid="domain-put"
            onClick={() =>
              run("domain_put", () =>
                invoke(domainCmd("domain_doc_put"), {
                  id: "kitchen-item-1",
                  body: JSON.stringify({ item: "oats", qty: 1 }),
                }),
              )
            }
          >
            Put domain doc
          </button>
          <button
            disabled={busy}
            data-testid="domain-list"
            onClick={() =>
              run("domain_list", () => invoke(domainCmd("domain_doc_list")))
            }
          >
            List domain docs
          </button>
        </div>
      </div>

      {error ? (
        <p className="error" data-testid="fixture-error">
          {error}
        </p>
      ) : null}

      <div className="panel">
        <strong>Log</strong>
        <pre data-testid="fixture-log">{log.join("\n") || "(empty)"}</pre>
      </div>
    </main>
  );
}

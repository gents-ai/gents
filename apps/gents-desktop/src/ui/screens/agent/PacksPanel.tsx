/* Packs for this agent: what is installed and whether it is current, the
   registry to find more, and the registry account. Every action is the same
   operation `gents pack` runs, through the desktop bridge. */
import { useCallback, useEffect, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { toast } from "sonner";
import { call, message } from "./bridgeCall";
import { ChoiceRow } from "./editors";
import { Group, Row } from "./rows";

type Edited = "refuse" | "overwrite" | "keep";

interface InstalledPack {
  pack: string;
  installed: string;
  latest: string | null;
  outdated: boolean;
}

interface FoundPack {
  namespace: string;
  name: string;
  description?: string | null;
  latest?: string | null;
  kind?: string;
}

/* An installed plugin that can call a model, and the profile it is bound to
   (none leaves it running without model calls). */
interface PluginSlot {
  plugin: string;
  slot: string;
  profile: string | null;
}

interface SlotProfile {
  profile_id: string;
  display_name?: string | null;
  model_name: string;
  usable: boolean;
}

/* An install or update that would replace documents someone edited stops
   and names them; the person chooses to keep or overwrite. */
function editedChoice(error: unknown): boolean {
  return message(error).includes("--overwrite");
}

export function PacksPanel() {
  const [installed, setInstalled] = useState<InstalledPack[] | null>(null);
  const [account, setAccount] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<FoundPack[]>([]);
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [slots, setSlots] = useState<PluginSlot[]>([]);
  const [profiles, setProfiles] = useState<SlotProfile[]>([]);

  const refresh = useCallback(async () => {
    try {
      const report = await call<{ packs: InstalledPack[] }>("desktop_pack_installed");
      setInstalled(report.packs);
    } catch (error) {
      setInstalled([]);
      toast.error(message(error));
    }
    try {
      const report = await call<{ plugins: PluginSlot[]; profiles: SlotProfile[] }>(
        "desktop_pack_plugin_slots",
      );
      setSlots(report.plugins);
      setProfiles(report.profiles.filter((profile) => profile.usable));
    } catch (error) {
      setSlots([]);
      toast.error(message(error));
    }
    try {
      const me = await call<{ account: { username: string } }>("desktop_pack_whoami");
      setAccount(me.account.username);
    } catch {
      setAccount(null);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function search(next: number) {
    setBusy("search");
    try {
      const report = await call<{ packs: FoundPack[]; has_more: boolean }>(
        "desktop_pack_search",
        {
          query,
          page: next,
        },
      );
      setFound(report.packs);
      setHasMore(report.has_more);
      setPage(next);
    } catch (error) {
      toast.error(message(error));
    } finally {
      setBusy(null);
    }
  }

  async function install(
    pack: string,
    edited: Edited = "refuse",
    grantAuthority = false,
  ) {
    setBusy(pack);
    try {
      await call("desktop_pack_install", {
        request: { package: pack, edited, grantAuthority },
      });
      toast.success(`Installed ${pack}`);
      await refresh();
    } catch (error) {
      const text = message(error);
      if (
        text.includes("--grant-authority") &&
        window.confirm(`${text}\n\nAllow it?`)
      ) {
        return install(pack, edited, true);
      }
      if (editedChoice(error)) {
        const overwrite = window.confirm(
          `${text}\n\nOK replaces them, Cancel keeps your edits.`,
        );
        return install(pack, overwrite ? "overwrite" : "keep", grantAuthority);
      }
      toast.error(text);
    } finally {
      setBusy(null);
    }
  }

  async function update(pack: string, edited: Edited = "refuse") {
    setBusy(pack);
    try {
      await call("desktop_pack_update", { package: pack, edited });
      toast.success(`Updated ${pack}`);
      await refresh();
    } catch (error) {
      if (editedChoice(error)) {
        const overwrite = window.confirm(
          `${message(error)}\n\nOK replaces them, Cancel keeps your edits.`,
        );
        return update(pack, overwrite ? "overwrite" : "keep");
      }
      toast.error(message(error));
    } finally {
      setBusy(null);
    }
  }

  async function remove(pack: string) {
    if (!window.confirm(`Remove ${pack} and everything it installed?`)) return;
    setBusy(pack);
    try {
      await call("desktop_pack_remove", { package: pack });
      toast.success(`Removed ${pack}`);
      await refresh();
    } catch (error) {
      toast.error(message(error));
    } finally {
      setBusy(null);
    }
  }

  async function bindSlot(plugin: string, profile: string) {
    setBusy(plugin);
    try {
      await call("desktop_pack_plugin_bind", {
        request: { plugin, profile: profile || null },
      });
      await refresh();
    } catch (error) {
      toast.error(message(error));
    } finally {
      setBusy(null);
    }
  }

  async function signIn() {
    setBusy("account");
    try {
      await call("desktop_pack_login", { token: token.trim() });
      setToken("");
      await refresh();
    } catch (error) {
      toast.error(message(error));
    } finally {
      setBusy(null);
    }
  }

  async function signOut() {
    await call("desktop_pack_logout").catch((error) => toast.error(message(error)));
    await refresh();
  }

  return (
    <div>
      <Group title="Installed">
        {installed === null && <Row label="Loading" />}
        {installed?.length === 0 && <Row label="No packs installed" />}
        {installed?.map((pack) => (
          <Row
            key={pack.pack}
            label={pack.pack}
            description={
              pack.outdated
                ? `${pack.installed}, ${pack.latest} available`
                : pack.installed
            }
          >
            <div className="flex gap-2">
              {pack.outdated && (
                <Button
                  size="sm"
                  disabled={busy !== null}
                  onClick={() => update(pack.pack)}
                >
                  Update
                </Button>
              )}
              <Button
                size="sm"
                variant="outline"
                disabled={busy !== null}
                onClick={() => remove(pack.pack)}
              >
                Remove
              </Button>
            </div>
          </Row>
        ))}
      </Group>

      {slots.length > 0 && (
        <Group title="Model calls">
          {slots.map((slot) => (
            <ChoiceRow
              key={slot.plugin}
              id={`slot-${slot.plugin}`}
              label={slot.plugin}
              description="Optional. Unset, the plugin runs without a model."
              value={slot.profile ?? ""}
              none="Not set"
              disabled={busy !== null}
              onChange={(profile) => void bindSlot(slot.plugin, profile)}
              items={profiles.map((profile) => ({
                value: profile.profile_id,
                label: profile.display_name || profile.model_name,
              }))}
            />
          ))}
        </Group>
      )}

      <Group title="Registry">
        <Row label="Search">
          <form
            className="flex gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void search(1);
            }}
          >
            <Input
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Packs and plugins"
              aria-label="Search the registry"
            />
            <Button type="submit" size="sm" disabled={busy !== null}>
              Search
            </Button>
          </form>
        </Row>
        {found.map((pack) => {
          const name = `${pack.namespace}/${pack.name}`;
          return (
            <Row
              key={name}
              label={name}
              description={pack.description ?? pack.latest ?? undefined}
            >
              <Button size="sm" disabled={busy !== null} onClick={() => install(name)}>
                Install
              </Button>
            </Row>
          );
        })}
        {(page > 1 || hasMore) && (
          <Row label={`Page ${page}`}>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={page === 1}
                onClick={() => search(page - 1)}
              >
                Previous
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={!hasMore}
                onClick={() => search(page + 1)}
              >
                Next
              </Button>
            </div>
          </Row>
        )}
      </Group>

      <Group title="Account">
        {account ? (
          <Row label={account} description="Signed in to the registry">
            <Button size="sm" variant="outline" onClick={signOut}>
              Sign out
            </Button>
          </Row>
        ) : (
          <Row label="Sign in" description="A token from your registry dashboard">
            <div className="flex gap-2">
              <Input
                type="password"
                value={token}
                onChange={(event) => setToken(event.target.value)}
                aria-label="Registry token"
              />
              <Button
                size="sm"
                disabled={!token.trim() || busy !== null}
                onClick={signIn}
              >
                Sign in
              </Button>
            </div>
          </Row>
        )}
      </Group>
    </div>
  );
}

import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import { toast } from "sonner";
import type {
  FoundPack,
  InstalledPack,
  PackEditedChoice,
  PackPluginSlot,
  PackSlotProfile,
  PackWireSources,
} from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { defaultAgentOf } from "@/lib/agents";
import type { NodeView } from "../../../hooks/fleetStore";
import { ChoiceRow } from "./editors";
import { Group, Row } from "./rows";
import { WirePackDialog, type WirePackDialogProps } from "./WirePackDialog";
import { RegistryPack } from "./RegistryPack";
import { errorText } from "../../../lib/errorText";
import { href } from "@/lib/router";
import { packContents } from "./packContents";

/** An install or update that would replace documents someone edited stops and
 * names them; the person chooses keep or overwrite. */
function editedChoice(error: unknown): boolean {
  return errorText(error).includes("--overwrite");
}

const NO_SOURCES: PackWireSources = { plugins: [], target_packs: [] };

export function PacksPanel({ deployment }: { deployment: NodeView }) {
  const { actions } = useApp();
  const [installed, setInstalled] = useState<InstalledPack[] | null>(null);
  const [account, setAccount] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [found, setFound] = useState<FoundPack[]>([]);
  const [page, setPage] = useState(1);
  const [hasMore, setHasMore] = useState(false);
  /* null until the first answer; a search is in flight while true */
  const [searched, setSearched] = useState<boolean | null>(null);
  const searches = useRef(0);
  /* the registry pack whose page is open under its line */
  const [opened, setOpened] = useState<string | null>(null);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [slots, setSlots] = useState<PackPluginSlot[]>([]);
  const [profiles, setProfiles] = useState<PackSlotProfile[]>([]);
  const [sources, setSources] = useState<PackWireSources>(NO_SOURCES);
  const [wiring, setWiring] = useState<Omit<
    WirePackDialogProps,
    "agents" | "agent" | "onClose"
  > | null>(null);

  /* a failed read is not an empty one */
  const [unread, setUnread] = useState<string | null>(null);
  const refresh = useCallback(async () => {
    try {
      const report = await actions.listInstalledPacks();
      setInstalled(report.packs);
      setUnread(null);
    } catch (error) {
      setInstalled([]);
      setUnread(errorText(error));
    }
    try {
      const report = await actions.listPackPluginSlots();
      setSlots(report.plugins);
      setProfiles(report.profiles.filter((profile) => profile.usable));
    } catch (error) {
      setSlots([]);
      toast.error(errorText(error));
    }
    try {
      setSources(await actions.listPackWireSources());
    } catch (error) {
      setSources(NO_SOURCES);
      toast.error(errorText(error));
    }
    try {
      const me = await actions.readPackAccount();
      setAccount(me.account.username);
    } catch {
      setAccount(null);
    }
  }, [actions]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  /* an empty query lists every pack, a page at a time; an answer to a search
     typed over since is dropped */
  async function search(next: number, q = query) {
    const mine = ++searches.current;
    setSearched((was) => (was === null ? null : true));
    try {
      const report = await actions.searchPacks(q.trim(), next);
      if (mine !== searches.current) return;
      setFound(report.packs);
      setHasMore(report.has_more);
      setPage(next);
    } catch (error) {
      if (mine === searches.current) toast.error(errorText(error));
    } finally {
      if (mine === searches.current) setSearched(false);
    }
  }

  /* the registry shows its packs as the page opens, and follows what is
     typed once typing pauses */
  const typed = useRef(false);
  useEffect(() => {
    const wait = typed.current ? 300 : 0;
    typed.current = true;
    const timer = setTimeout(() => void search(1, query), wait);
    return () => clearTimeout(timer);
    /* only a change of query searches again; search reads the rest fresh */
  }, [query]);

  async function install(
    pack: string,
    edited: PackEditedChoice = "refuse",
    grantAuthority = false,
  ) {
    setBusy(pack);
    try {
      await actions.installPack({ package: pack, edited, grantAuthority });
      toast.success(`Installed ${pack}`);
      await refresh();
    } catch (error) {
      const text = errorText(error);
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

  async function update(pack: string, edited: PackEditedChoice = "refuse") {
    setBusy(pack);
    try {
      await actions.updatePack(pack, edited);
      toast.success(`Updated ${pack}`);
      await refresh();
    } catch (error) {
      if (editedChoice(error)) {
        const overwrite = window.confirm(
          `${errorText(error)}\n\nOK replaces them, Cancel keeps your edits.`,
        );
        return update(pack, overwrite ? "overwrite" : "keep");
      }
      toast.error(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function remove(pack: string) {
    if (!window.confirm(`Remove ${pack} and everything it installed?`)) return;
    setBusy(pack);
    try {
      await actions.removePack(pack);
      toast.success(`Removed ${pack}`);
      await refresh();
    } catch (error) {
      toast.error(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function bindSlot(plugin: string, profile: string) {
    setBusy(plugin);
    try {
      await actions.bindPackPlugin(plugin, profile || null);
      await refresh();
    } catch (error) {
      toast.error(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function signIn() {
    setBusy("account");
    try {
      await actions.signInToPackRegistry(token.trim());
      setToken("");
      await refresh();
    } catch (error) {
      toast.error(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  async function signOut() {
    await actions
      .signOutOfPackRegistry()
      .catch((error) => toast.error(errorText(error)));
    await refresh();
  }

  /* the buttons that open the dialog for an installed pack */
  function enableFor(pack: string) {
    const plugins = sources.plugins.filter((p) => p.pack === pack);
    if (plugins.length > 0) {
      return plugins.map(({ plugin }) => (
        <Button
          key={plugin}
          size="sm"
          variant="outline"
          disabled={busy !== null}
          onClick={() => setWiring({ plugin, targetsFrom: [] })}
        >
          Enable for agent
        </Button>
      ));
    }
    return null;
  }

  return (
    <div>
      {wiring && (
        <WirePackDialog
          {...wiring}
          agents={deployment.agents}
          agent={defaultAgentOf(deployment)?.agentDid}
          onClose={(wired) => {
            setWiring(null);
            if (wired) void refresh();
          }}
        />
      )}
      <Group title="Installed">
        {installed === null && <Row label="Loading" />}
        {unread !== null && (
          <Row label="Couldn't read the installed packs" description={unread} />
        )}
        {unread === null && installed?.length === 0 && (
          <Row label="No packs installed" />
        )}
        {installed?.map((pack) => (
          <Row
            key={pack.pack}
            label={pack.pack}
            description={
              <>
                {pack.outdated ? (
                  `${pack.installed}, ${pack.latest} available`
                ) : pack.error ? (
                  <span title={pack.error}>
                    {pack.installed} · couldn't check for updates
                  </span>
                ) : (
                  pack.installed
                )}
                <PackAdds deployment={deployment} pack={pack.pack} />
              </>
            }
          >
            <div className="flex gap-2">
              {enableFor(pack.pack)}
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
            className="flex items-center gap-2"
            onSubmit={(event) => {
              event.preventDefault();
              void search(1);
            }}
          >
            <Input
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Packs and plugins"
              aria-label="Search the registry"
            />
          </form>
        </Row>
        {searched === null && (
          <Row label="Loading the registry…">
            <Spinner />
          </Row>
        )}
        {searched === false && found.length === 0 && (
          <Row
            label={query.trim() ? "No packs match" : "The registry has no packs yet"}
          >
            {null}
          </Row>
        )}
        {found.length > 0 && (
          /* one line a pack, in a box of its own height, so a long registry
             scrolls inside it rather than stretching the page */
          <ul
            aria-label="Registry packs"
            className="max-h-80 divide-y divide-border/40 overflow-y-auto"
          >
            {found.map((pack) => {
              const name = `${pack.namespace}/${pack.name}`;
              return (
                <RegistryPack
                  key={name}
                  pack={pack}
                  installed={installed?.some((p) => p.pack === name) ?? false}
                  installing={busy !== null}
                  onInstall={() => install(name)}
                  onKeyword={(word) => {
                    setOpened(null);
                    setQuery(word);
                  }}
                  open={opened === name}
                  onToggle={() => setOpened((was) => (was === name ? null : name))}
                />
              );
            })}
          </ul>
        )}
        {(page > 1 || hasMore) && (
          <Row label={`Page ${page}`}>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant="outline"
                disabled={page === 1 || searched === true}
                onClick={() => search(page - 1)}
              >
                Previous
              </Button>
              <Button
                size="sm"
                variant="outline"
                disabled={!hasMore || searched === true}
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
            <div className="flex items-center gap-2">
              <Input
                type="password"
                value={token}
                onChange={(event) => setToken(event.target.value)}
                placeholder="Registry token"
                aria-label="Registry token"
              />
              <Button disabled={!token.trim() || busy !== null} onClick={signIn}>
                Sign in
              </Button>
            </div>
          </Row>
        )}
      </Group>
    </div>
  );
}

/* what a pack installed, each kind linked to the section that lists it */
function PackAdds({ deployment, pack }: { deployment: NodeView; pack: string }) {
  const kinds = packContents(deployment, pack);
  if (!kinds.length) return null;
  return (
    <span className="block">
      Adds{" "}
      {kinds.map((kind, i) => (
        <Fragment key={kind.section}>
          {i > 0 && " · "}
          <a
            href={href({
              name: "agent",
              nodeDid: deployment.nodeDid,
              section: kind.section,
            })}
            className="text-foreground underline-offset-2 hover:underline"
          >
            {kind.count} {kind.count === 1 ? kind.one : kind.many}
          </a>
        </Fragment>
      ))}
    </span>
  );
}

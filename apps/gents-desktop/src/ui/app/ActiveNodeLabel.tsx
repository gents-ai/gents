import { useHomeDid, useSelectedNode, useSelectedNodeDid } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { href } from "@/lib/router";
import { cn } from "@gents/ui/lib/utils";

export function ActiveNodeLabel({
  composer = false,
  className,
}: {
  composer?: boolean;
  className?: string;
}) {
  const node = useSelectedNode();
  const selectedDid = useSelectedNodeDid();
  const homeDid = useHomeDid();
  const name = node?.node.displayName ?? node?.label;
  const label = node
    ? `${isLocalNode(node, homeDid) ? "Local" : "Remote"} · ${name}${node.label !== name ? ` on ${node.label}` : ""}`
    : selectedDid
      ? "Node unavailable"
      : "Select a node";
  return (
    <a
      href={href(
        node
          ? { name: "agent", nodeDid: node.nodeDid, section: "agent" }
          : { name: "nodes" },
      )}
      aria-label={composer ? "Message destination" : "Active node"}
      title={selectedDid ? `${label} · ${selectedDid}` : label}
      className={cn(
        "flex min-w-0 max-w-52 items-center gap-1.5 truncate rounded-md px-1 py-1 text-xs text-muted-foreground hover:bg-accent hover:text-foreground",
        className,
      )}
    >
      <span className="truncate">{label}</span>
      {selectedDid && (
        <span className="shrink-0 font-mono text-[10px] opacity-70">
          {selectedDid.slice(-8)}
        </span>
      )}
    </a>
  );
}

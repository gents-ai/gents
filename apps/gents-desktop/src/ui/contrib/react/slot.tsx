import { ContribBoundary, ContribRender } from "./boundary";
import { useContributions } from "./use-contributions";

/* a bar area: every rendered contribution inline, in order, each behind its
   own boundary; nothing when the area is empty */
export function Slot({ area }: { area: string }) {
  const items = useContributions(area);
  if (items.length === 0) return null;
  return (
    <>
      {items.map((c) =>
        c.render ? (
          <ContribBoundary
            id={c.id}
            key={`${c.source ?? "core"}:${c.id}`}
            variant="chip"
          >
            <ContribRender render={c.render} />
          </ContribBoundary>
        ) : null,
      )}
    </>
  );
}

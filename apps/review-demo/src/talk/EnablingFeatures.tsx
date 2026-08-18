const FEATURES = [
  {
    title: "Collection tools",
    body: (
      <>
        A <code>DatastoreToolSurface</code> grants named create and read tools onto a collection.
        The model calls <code>write_review_area</code> or <code>read_candidate_finding</code>; the
        runtime does one validated write or a bound collection read.
      </>
    ),
    timeline: (
      <>
        Inline <code>write_tools</code>{" "}
        <a href="https://github.com/source-inc/gents/pull/431">#431</a> (Jun 8). Reusable surfaces{" "}
        <a href="https://github.com/source-inc/gents/pull/1081">#1081</a> (Aug 8). Query bindings
        Aug 17.
      </>
    ),
    enables: "Each stage emits the next typed row. Those creates are the edges the other two features consume.",
  },
  {
    title: "Templated task prompts",
    body: (
      <>
        A Task’s <code>prompt.md</code> renders <code>{"{{ doc.* }}"}</code> and{" "}
        <code>{"{{ event.* }}"}</code> from the source document. The row is the assignment.
      </>
    ),
    timeline: (
      <>
        Apr 22 — <a href="https://github.com/source-inc/gents/pull/63">#63</a>, sidecars{" "}
        <a href="https://github.com/source-inc/gents/issues/67">#67</a>, <code>args.*</code>{" "}
        <a href="https://github.com/source-inc/gents/pull/70">#70</a> — then unused until the Aug
        packs. Updates: cache-safe <a href="https://github.com/source-inc/gents/pull/506">#506</a>{" "}
        (Jun 15), <code>group.*</code> in{" "}
        <a href="https://github.com/source-inc/gents/pull/1113">#1113</a> (Aug 13). First pack use{" "}
        <a href="https://github.com/source-inc/gents/pull/1081">#1081</a>.
      </>
    ),
    enables: (
      <>
        A <code>ReviewArea</code> write becomes a self-contained scanner prompt. Verify uses{" "}
        <code>{"{{ group.docs }}"}</code> — that slot did not exist in April.
      </>
    ),
  },
  {
    title: "Task triggers",
    body: (
      <>
        A Task is fired by a document: Schedule, Manual, or EventTrigger on a collection create.
        Grouping is the latest increment.
      </>
    ),
    timeline: (
      <>
        Designed Apr 21. Engine <a href="https://github.com/source-inc/gents/pull/63">#63</a> +
        EventTrigger <a href="https://github.com/source-inc/gents/pull/68">#68</a> + manual{" "}
        <a href="https://github.com/source-inc/gents/pull/70">#70</a>. Write→fire{" "}
        <a href="https://github.com/source-inc/gents/pull/431">#431</a> (Jun 8). CLI{" "}
        <a href="https://github.com/source-inc/gents/pull/474">#474</a>. Filter validation{" "}
        <a href="https://github.com/source-inc/gents/pull/1034">#1034</a>. Pack graphs{" "}
        <a href="https://github.com/source-inc/gents/pull/1081">#1081</a>. Correlation /{" "}
        <code>per_group</code> <a href="https://github.com/source-inc/gents/issues/1096">#1096</a> /{" "}
        <a href="https://github.com/source-inc/gents/pull/1113">#1113</a> (Aug 13).
      </>
    ),
    enables: (
      <>
        Seed a <code>ReviewJob</code> and the graph runs itself. Surfaces write members; templates
        brief each worker; the trigger is the edge — and now the join.
      </>
    ),
  },
];

export function EnablingFeatures() {
  return (
    <section className="talk-block">
      <p className="eyebrow">Enabling features</p>
      {FEATURES.map((feature) => (
        <article key={feature.title} className="feature-card">
          <h3>{feature.title}</h3>
          <p>{feature.body}</p>
          <p className="feature-meta">
            <strong>Timeline.</strong> {feature.timeline}
            <br />
            <strong>Enables.</strong> {feature.enables}
          </p>
        </article>
      ))}
    </section>
  );
}

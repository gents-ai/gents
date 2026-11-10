export interface IndependentNodeFixture {
  readonly nodeDid: string;
  readonly baseUrl: string;
  readonly dataRoot: string | undefined;
  dispose(): Promise<void>;
}

async function disposeAll(fixtures: IndependentNodeFixture[]): Promise<unknown[]> {
  const results = await Promise.allSettled(
    fixtures.map((fixture) => Promise.resolve().then(() => fixture.dispose())),
  );
  return results.flatMap((result) =>
    result.status === "rejected" ? [result.reason] : [],
  );
}

/** Starts two independently owned runtimes; a failure never leaves the first running. */
export async function startTwoIndependentNodes<T extends IndependentNodeFixture>(
  start: () => Promise<T>,
): Promise<{ first: T; second: T; dispose(): Promise<void> }> {
  const fixtures: T[] = [];
  try {
    fixtures.push(await start());
    fixtures.push(await start());
    const [first, second] = fixtures;
    if (
      !first.nodeDid ||
      !second.nodeDid ||
      first.nodeDid === second.nodeDid ||
      !first.baseUrl ||
      !second.baseUrl ||
      first.baseUrl === second.baseUrl ||
      !first.dataRoot ||
      !second.dataRoot ||
      first.dataRoot === second.dataRoot
    ) {
      throw new Error("Two independent nodes and data homes are required.");
    }
    let disposal: Promise<void> | undefined;
    return {
      first,
      second,
      dispose() {
        disposal ??= disposeAll(fixtures).then((errors) => {
          if (errors.length)
            throw new AggregateError(errors, "Could not stop both nodes.");
        });
        return disposal;
      },
    };
  } catch (error) {
    const errors = await disposeAll(fixtures);
    if (errors.length)
      throw new AggregateError([error, ...errors], "Node startup and cleanup failed.");
    throw error;
  }
}

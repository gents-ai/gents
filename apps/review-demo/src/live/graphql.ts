/** Must match `gents_protocol::graphql::escape_graphql_string`. */
export function escapeGraphqlString(value: string): string {
  return value
    .replaceAll("\\", "\\\\")
    .replaceAll('"', '\\"')
    .replaceAll("\n", "\\n")
    .replaceAll("\r", "\\r")
    .replaceAll("\t", "\\t");
}

export async function postGraphql<T>(query: string): Promise<T> {
  const response = await fetch("/api/v0/graphql", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ query }),
  });
  if (!response.ok) {
    throw new Error(`graphql HTTP ${response.status}`);
  }
  const payload = (await response.json()) as {
    data?: T;
    errors?: { message: string }[];
  };
  if (payload.errors?.length) {
    throw new Error(payload.errors.map((error) => error.message).join("; "));
  }
  if (!payload.data) {
    throw new Error("graphql returned no data");
  }
  return payload.data;
}

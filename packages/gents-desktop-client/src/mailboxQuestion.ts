import type {
  MailboxItemView,
  MailboxQuestion,
  MailboxQuestionOption,
} from "./types.js";

const text = (value: unknown): value is string =>
  typeof value === "string" && value.trim() !== "";
const flag = (value: unknown) =>
  value === undefined || typeof value === "boolean";
const only = (value: object, keys: string[]) =>
  Object.keys(value).every((key) => keys.includes(key));

function option(value: unknown): MailboxQuestionOption | null {
  if (typeof value !== "object" || value === null) return null;
  if (!only(value, ["id", "label", "description"])) return null;
  const { id, label, description } = value as Record<string, unknown>;
  if (!text(id) || !text(label)) return null;
  if (
    description !== undefined &&
    description !== null &&
    typeof description !== "string"
  ) {
    return null;
  }
  return { id, label, description: description ?? null };
}

export function parseMailboxQuestion(
  item: MailboxItemView,
): MailboxQuestion | null {
  if (item.kind !== "ask" || item.action !== "start_request" || !item.payload) {
    return null;
  }
  let value: unknown;
  try {
    value = JSON.parse(item.payload);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null || Array.isArray(value))
    return null;
  if (
    !only(value, [
      "version",
      "prompt",
      "options",
      "multi_select",
      "allow_free_text",
    ])
  ) {
    return null;
  }
  const { version, prompt, options, multi_select, allow_free_text } =
    value as Record<string, unknown>;
  if (version !== 1 || !text(prompt) || !Array.isArray(options)) return null;
  if (options.length < 2 || options.length > 4) return null;
  if (!flag(multi_select) || !flag(allow_free_text)) return null;
  const parsed = options.map(option);
  if (parsed.some((entry) => entry === null)) return null;
  const valid = parsed as MailboxQuestionOption[];
  if (new Set(valid.map((entry) => entry.id.trim())).size !== valid.length)
    return null;
  return {
    version,
    prompt,
    options: valid,
    multi_select: multi_select === true,
    allow_free_text: allow_free_text === true,
  };
}

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
  if (!only(value, ["id", "label", "description", "prompt"])) return null;
  const { id, label, description, prompt } = value as Record<string, unknown>;
  if (!text(id) || !text(label)) return null;
  if (
    description !== undefined &&
    description !== null &&
    typeof description !== "string"
  ) {
    return null;
  }
  if (prompt !== undefined && prompt !== null && !text(prompt)) return null;
  return {
    id,
    label,
    description: description ?? null,
    ...(prompt === undefined ? {} : { prompt: prompt ?? null }),
  };
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
      "optional",
      "recommended_order",
    ])
  ) {
    return null;
  }
  const {
    version,
    prompt,
    options,
    multi_select,
    allow_free_text,
    optional,
    recommended_order,
  } = value as Record<string, unknown>;
  if (
    (version !== 1 && version !== 2 && version !== 3) ||
    !text(prompt) ||
    !Array.isArray(options)
  )
    return null;
  if (optional !== null && !flag(optional)) return null;
  if (version === 1 && optional === true) return null;
  if (!(
    (options.length >= 2 && options.length <= 4) ||
    (version >= 2 && options.length === 0 && allow_free_text === true)
  ))
    return null;
  if (!flag(multi_select) || !flag(allow_free_text)) return null;
  const parsed = options.map(option);
  if (parsed.some((entry) => entry === null)) return null;
  const valid = parsed as MailboxQuestionOption[];
  if (new Set(valid.map((entry) => entry.id.trim())).size !== valid.length)
    return null;
  if (
    valid.some((entry) => entry.prompt != null) &&
    (version !== 3 || allow_free_text !== true || multi_select === true)
  )
    return null;
  let recommendedOrder: string[] | null | undefined;
  if (recommended_order === null) recommendedOrder = null;
  else if (recommended_order !== undefined) {
    if (
      version !== 3 ||
      multi_select !== true ||
      valid.length === 0 ||
      !Array.isArray(recommended_order) ||
      recommended_order.length !== valid.length ||
      !recommended_order.every(text)
    )
      return null;
    const ids = recommended_order.map((id) => id.trim());
    if (
      new Set(ids).size !== valid.length ||
      ids.some((id) => !valid.some((entry) => entry.id.trim() === id))
    )
      return null;
    recommendedOrder = ids.map(
      (id) => valid.find((entry) => entry.id.trim() === id)!.id,
    );
  }
  return {
    version,
    prompt,
    options: valid,
    multi_select: multi_select === true,
    allow_free_text: allow_free_text === true,
    ...(recommended_order === undefined
      ? {}
      : { recommended_order: recommendedOrder }),
    ...(optional === undefined
      ? {}
      : { optional: optional === null ? null : optional === true }),
  };
}

import { expect, it } from "vitest";
import { parseMailboxQuestion } from "./mailboxQuestion.js";
import type { MailboxItemView } from "./types.js";
const payload = {
  version: 1,
  prompt: "Choose an approach",
  options: [
    { id: "small", label: "Small change", description: "Less disruption" },
    { id: "broad", label: "Broader change" },
  ],
  allow_free_text: true,
};
const item = (value: unknown) =>
  ({
    kind: "ask",
    action: "start_request",
    payload: JSON.stringify(value),
  }) as MailboxItemView;
it("decodes the existing native question without selecting an answer", () => {
  expect(parseMailboxQuestion(item(payload))).toEqual({
    ...payload,
    options: [
      { ...payload.options[0] },
      { ...payload.options[1], description: null },
    ],
    multi_select: false,
  });
});
it.each([
  null,
  [],
  { ...payload, version: 3 },
  { ...payload, prompt: " " },
  { ...payload, options: [] },
  { ...payload, options: Array(5).fill(payload.options[0]) },
  {
    ...payload,
    options: [
      { id: "same", label: "One" },
      { id: " same ", label: "Two" },
    ],
  },
  {
    ...payload,
    options: [{ id: "a", label: "One", action: "execute" }, payload.options[1]],
  },
  { ...payload, allow_free_text: "yes" },
  { ...payload, multi_select: "yes" },
  { ...payload, optional: true },
])("refuses unsupported or malformed question payload %#", (value) =>
  expect(parseMailboxQuestion(item(value))).toBeNull(),
);
it("does not treat informational mailbox items as questions", () => {
  expect(parseMailboxQuestion({ ...item(payload), kind: "info" })).toBeNull();
  expect(
    parseMailboxQuestion({ ...item(payload), action: "write_document" }),
  ).toBeNull();
  expect(parseMailboxQuestion({ ...item(payload), payload: "{" })).toBeNull();
});

it("decodes optional and required version-two questions without inventing an answer", () => {
  for (const optional of [true, false, null]) {
    expect(
      parseMailboxQuestion(item({ ...payload, version: 2, optional }))
        ?.optional,
    ).toBe(optional);
  }
  expect(
    parseMailboxQuestion(
      item({ ...payload, version: 2, options: [], optional: true }),
    ),
  ).toEqual({
    version: 2,
    prompt: payload.prompt,
    options: [],
    optional: true,
    multi_select: false,
    allow_free_text: true,
  });
  expect(
    parseMailboxQuestion(
      item({ ...payload, version: 2, options: [], allow_free_text: false }),
    ),
  ).toBeNull();
  expect(
    parseMailboxQuestion(item({ ...payload, version: 2, optional: "yes" })),
  ).toBeNull();
});

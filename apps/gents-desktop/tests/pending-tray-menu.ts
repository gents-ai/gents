import { fireEvent, screen, within } from "@testing-library/react";

export async function moveItem(row: HTMLElement, direction: "up" | "down") {
  fireEvent.click(within(row).getByRole("button", { name: "More for this message" }));
  return screen.findByRole("menuitem", { name: `Move pending message ${direction}` });
}

export async function moveDisabled(row: HTMLElement, direction: "up" | "down") {
  const item = await moveItem(row, direction);
  const disabled = item.getAttribute("aria-disabled") === "true";
  fireEvent.keyDown(item, { key: "Escape" });
  return disabled;
}

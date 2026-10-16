/* Items that name a page being made rather than a saved document, so a
   draft is a place in the history. This desktop's ids are names or `newId`
   ids, which never start with "~"; core does not forbid one. */
export const NEW_ITEM = "~new";

const ADD_PREFIX = "~add:";

export const addItem = (provider: string) => `${ADD_PREFIX}${provider}`;

export const addingFrom = (item: string | undefined) =>
  item?.startsWith(ADD_PREFIX) ? item.slice(ADD_PREFIX.length) : null;

# Mailbox surface asset

## Installation and authority

`gents pack install mailbox --home <home>` materializes this reusable surface
asset under the home's pack assets; it is not a complete desired-state root and
does not write runtime configuration. Incorporate the surface into a document
pack's canonical `pack_config.json`, then reference `mailbox-writes` from the
intended context's `Tools.datastore` configuration after reviewing its declared
fields. There is no graph or seed in this asset pack.

The surface grants the stamped `file_mailbox_item` tool. Packs copy or reference
it and explicitly attach `mailbox-writes` only to contexts whose agents may ask
their human owner for attention. It is not granted by default.

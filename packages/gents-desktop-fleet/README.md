# @source-inc/gents-desktop-fleet

Peer discovery, pairing, QR import, network health, fleet rows, and semantic
styles. `BrandLockup` and theme controls stay host-owned through slots.

Local runtime and inference administration are opt-in:

```ts
import { FleetDashboard } from "@source-inc/gents-desktop-fleet";
import {
  InferenceSetupWizard,
  LocalRuntimeConnect,
} from "@source-inc/gents-desktop-fleet/local-runtime";
```

```css
@import "@source-inc/gents-desktop-tokens/semantic.css";
@import "@source-inc/gents-desktop-ui/styles.css";
@import "@source-inc/gents-desktop-fleet/styles.css";
/* Only InferenceSetupWizard/LocalRuntimeConnect need this subpath: */
@import "@source-inc/gents-desktop-fleet/local-runtime.css";
/* Host semantic-token overrides come last. */
```

`FleetDashboard` and its `renderInferenceSetup` callout require only the base
fleet stylesheet. The packaged `InferenceSetupWizard` additionally requires
`local-runtime.css`.

White-label hosts can pass `FleetDashboard.copy.pairingQrHint` and
`LocalRuntimeConnect.copy.runtimeProductName` / `cliBinaryName`; Gents CLI
wording is only the first-party default.

**Required grants:** default + fleet-read; add fleet-admin for pairing UI.
Only hosts rendering the `local-runtime` subpath add runtime-admin.

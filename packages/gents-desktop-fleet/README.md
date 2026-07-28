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

**Required grants:** default + fleet-read; add fleet-admin for pairing UI.
Only hosts rendering the `local-runtime` subpath add runtime-admin.

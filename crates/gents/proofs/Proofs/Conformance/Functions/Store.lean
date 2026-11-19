import Proofs.Store.Identity.ClientDelegation
import Proofs.Store.Identity.GrantSets
import Proofs.Store.Identity.EnrollmentGrant
import Proofs.Store.SchemaInstall
import Proofs.Store.AccessClassification
import Proofs.Conformance.Family

namespace Identity.GrantSets

deriving instance Lean.ToJson for GrantSet

attribute [conformance_vocabulary] GrantSet.name

attribute [conformance_function] GrantSet.nativeAdmin GrantSet.permissions

end Identity.GrantSets

namespace Identity.EnrollmentGrant

deriving instance Lean.ToJson for ApprovalStep, Standing, SettlementStep, AfterRemoval,
  RestorationStep

attribute [conformance_function] afterGrant observeMemberAdministrator settlementStep afterRemoval
  permissionGrantAdmitted restorationStep

end Identity.EnrollmentGrant

namespace SchemaInstall

deriving instance Lean.ToJson for Live, Decision

attribute [conformance_function] decide requestAdmitted

end SchemaInstall

namespace AccessClassification

deriving instance Lean.ToJson for Classification, Failure, Erasure, StoreWrapper,
  InferenceWrapper, SessionWrapper, P2pWrapper, ServiceWrapper, ToolsWrapper, DesktopCoreWrapper, AutomationWrapper, DesktopBridgeWrapper,
  StoreRetryWrapper, ProtocolRetryWrapper, OwnerProbe, StoreOwnerWrapper, PassthroughWrapper,
  Printer, EvalWrapper, ToolcallsWrapper

attribute [conformance_function] storeWrapped inferenceWrapped sessionWrapped p2pWrapped
  serviceWrapped toolsWrapped desktopCoreWrapped automationWrapped desktopBridgeWrapped storeRetryWrapped protocolRetryWrapped
  storeOwnerFound passthroughWrapped passthroughPrinted evalWrapped toolcallsWrapped

end AccessClassification

attribute [conformance_function] Identity.ClientDelegation.credentialForActor

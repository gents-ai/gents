import Proofs.Mailbox.Properties

namespace Mailbox

def allStatuses : List Status :=
  [.open, .acted, .dismissed, .expired]

def allKinds : List Kind :=
  [.ask, .gate, .finished, .failed, .flag]

def allHandlings : List Handling :=
  [.ack, .startRequest, .writeDocument]

def allSourceKinds : List SourceKind :=
  [.graph, .session, .agent, .runtime, .tool]

def statusVocabulary : List String :=
  allStatuses.map Status.toDefraDB

def kindVocabulary : List String :=
  allKinds.map Kind.toDefraDB

def handlingVocabulary : List String :=
  allHandlings.map Handling.toDefraDB

def sourceKindVocabulary : List String :=
  allSourceKinds.map SourceKind.toDefraDB

end Mailbox

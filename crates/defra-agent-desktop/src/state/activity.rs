#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Chat,
    Operator,
    Peers,
    Logs,
}

impl Activity {
    pub const ALL: [Self; 4] = [Self::Chat, Self::Operator, Self::Peers, Self::Logs];

    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Operator => "Operator",
            Self::Peers => "Peers",
            Self::Logs => "Logs",
        }
    }

    pub fn nav_hint(self) -> &'static str {
        match self {
            Self::Chat => "conversations",
            Self::Operator => "config + runtime",
            Self::Peers => "pairing + identity",
            Self::Logs => "diagnostics",
        }
    }

    pub fn nav_badge(self) -> &'static str {
        match self {
            Self::Chat => "CH",
            Self::Operator => "OP",
            Self::Peers => "PP",
            Self::Logs => "LG",
        }
    }

    pub fn sidebar_width(self) -> f32 {
        match self {
            Self::Chat => 308.0,
            Self::Operator | Self::Peers => 292.0,
            Self::Logs => 272.0,
        }
    }

    pub fn rail_width(self) -> Option<f32> {
        match self {
            Self::Chat => None,
            Self::Operator => Some(400.0),
            Self::Peers => Some(380.0),
            Self::Logs => Some(360.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingChatAction {
    SelectDeployment { peer_id: String, agent_did: String },
    SelectConversation { session_id: String },
    CreateConversation,
    SubmitComposer,
    RetryLatestRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingShellAction {
    Navigate(Activity),
    OpenPeersSetup,
    Chat(PendingChatAction),
}

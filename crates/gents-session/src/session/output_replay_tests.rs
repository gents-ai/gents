use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::message::Message;
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TranscriptMessage,
};
use gents_protocol::rendered_request::RenderedRequestSource;
use gents_store::identity::NodeIdentity;

use super::output::replay_candidates::load_canonical_assistant_candidates;
use super::output::replay_request::ReplayRequestReader;
use super::output::CanonicalReplayScope;
use crate::session::provider_context_reduction::capture_source_boundary;
use gents_protocol::output::rows::{
    output_segment_create_variables, transcript_message_create_variables,
    CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};
use gents_protocol::transcript::SourceBoundary;

use gents_loop::claude_messages_body::{
    replay_stage, select_replay, ReplayIssuer, ReplayTag, ReplayWire, TaggedAssistantRow,
};
use gents_loop::provider_input::replay_frontier::flatten;


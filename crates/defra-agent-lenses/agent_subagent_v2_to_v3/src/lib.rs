//! Lens v2→v3: adds subagent extensions to AgentToolCall, AgentRequest,
//! and ToolSelection. Forward transform populates new fields with their
//! defaults; inverse transform drops them for P2P backward-compat.
//!
//! Operates over the same JSON-document iterator API as the v1→v2 lens.

use std::collections::HashMap;
use std::error::Error;

use lens_sdk::StreamOption;
use serde_json::Value;

lens_sdk::define!(try_transform, try_inverse);

fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    // Forward transform — implemented in Task 2.
    for item in iter {
        let input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };
        let _ = input;
        return Ok(StreamOption::Some(HashMap::new()));
    }
    Ok(StreamOption::EndOfStream)
}

fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    // Inverse transform — implemented in Task 2.
    for item in iter {
        let input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };
        let _ = input;
        return Ok(StreamOption::Some(HashMap::new()));
    }
    Ok(StreamOption::EndOfStream)
}

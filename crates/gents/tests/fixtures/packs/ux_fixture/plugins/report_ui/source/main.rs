// A ux module producer: for the input `{"role":"ux", ...}` prints the ESM
// module (and css) the desktop loads as the `report` UX plugin; for any
// other input, echoes it back so the plugin is also a plain echo tool. No
// JSON crate: a substring test on one field is the whole dispatch.

use std::io::{Read, Write};

const MODULE: &str = r#"import { AGENT_SECTIONS_AREA, TRANSCRIPT_DIRECTIVE_AREA } from '@gents/ux-sdk'
import { jsx } from 'react/jsx-runtime'

function Page({ agentDid }) {
  return jsx('p', { className: 'ux-fixture-report', children: `report for ${agentDid}` })
}

export default {
  id: 'report',
  name: 'Fixture report',
  defaultEnabled: false,
  register(ctx) {
    ctx.registerMany([
      { id: 'page', area: AGENT_SECTIONS_AREA, data: { group: 'Packs', label: 'Report', render: (props) => jsx(Page, props) } },
      { id: 'directive', area: TRANSCRIPT_DIRECTIVE_AREA, data: { name: 'report', render: ({ attrs }) => jsx('b', { children: attrs.id ?? '?' }) } },
    ])
  }
}
"#;

const CSS: &str = ".ux-fixture-report { font-style: italic }";

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("reading stdin");
    let output = if input.contains("\"role\"") && input.contains("\"ux\"") {
        format!("{{\"module\":{},\"css\":{}}}", json_string(MODULE), json_string(CSS))
    } else if input.trim().is_empty() {
        "null".to_owned()
    } else {
        input
    };
    std::io::stdout().write_all(output.as_bytes()).expect("writing stdout");
}

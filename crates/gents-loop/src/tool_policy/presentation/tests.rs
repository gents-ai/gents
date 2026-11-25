use super::*;
use crate::test_support::fixed_tool::FixedTool;

fn surface(names: &[&'static str]) -> Vec<Box<dyn ToolDyn>> {
    names
        .iter()
        .map(|name| {
            Box::new(FixedTool::new(name, "called").with_description("contract"))
                as Box<dyn ToolDyn>
        })
        .collect()
}

#[test]
fn admission_conforms_to_lean() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    enum AdmissionDecision {
        Allowed,
        Hidden,
        Unavailable,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum LeanOrigin {
        Provider,
        Composition,
    }
    gents_lean_vocab::families::tool_presentation_admission_decision::replay::<
        AdmissionDecision,
        LeanOrigin,
    >(|case| {
        let names: &[&'static str] = if case.granted {
            &["script", "target"]
        } else {
            &["script"]
        };
        let listed: Vec<String> = if case.hidden {
            vec!["target".into()]
        } else {
            Vec::new()
        };
        let tools = present_tools(surface(names), &listed);
        let origin = match case.origin {
            LeanOrigin::Provider => Origin::Provider,
            LeanOrigin::Composition => Origin::Composition,
        };
        match admit(&tools, "target", origin) {
            Ok(()) => AdmissionDecision::Allowed,
            Err(AdmissionRefusal::Hidden { .. }) => AdmissionDecision::Hidden,
            Err(AdmissionRefusal::Unavailable { .. }) => AdmissionDecision::Unavailable,
        }
    });
}

#[test]
fn effective_hiding_conforms_to_lean() {
    gents_lean_vocab::families::tool_presentation_effective_hidden::replay(|case| {
        let target = if case.gateway {
            "spawn_process"
        } else {
            "query"
        };
        let mut names = vec![target];
        if case.script_present {
            names.push("script");
        }
        let listed: Vec<String> = if case.listed {
            vec![target.into()]
        } else {
            Vec::new()
        };
        present_tools(surface(&names), &listed)
            .iter()
            .find(|tool| tool.name() == target)
            .is_some_and(|tool| tool.hidden())
    });
}

#[test]
fn gateways_conform_to_lean() {
    for case in gents_lean_vocab::families::tool_presentation_gateways::cases() {
        case.check(is_gateway(&case.input));
    }
}

#[test]
fn default_hidden_tools_conform_to_lean() {
    for case in gents_lean_vocab::families::tool_presentation_defaults::cases() {
        case.check(default_hidden(&case.input));
    }
}

#[tokio::test]
async fn hiding_keeps_the_tool_callable_and_names_it_once_in_script() {
    let shown = present_tools(surface(&["script", "query"]), &[]);
    assert_eq!(
        shown[0].definition(String::new()).await.description,
        "contract"
    );
    let tools = present_tools(
        surface(&["script", "query", "read_file"]),
        &["query".into()],
    );
    assert!(!tools[0].hidden());
    assert!(tools[1].hidden());
    assert!(!tools[2].hidden());
    assert_eq!(
        tools[0].definition(String::new()).await.description,
        "contract\nOffered only here, not as direct tools: query. Read `tools.NAME.description` and `tools.NAME.parameters` before the first call, then `await tools.NAME({...})`."
    );
    assert_eq!(
        tools[1].definition(String::new()).await.description,
        "contract"
    );
    assert_eq!(
        tools[1]
            .call("{}".into(), &ToolCallEnvelope::default())
            .await
            .unwrap(),
        "called"
    );
    let nothing_listed_is_present =
        present_tools(surface(&["script", "query"]), &["absent".into()]);
    assert_eq!(
        nothing_listed_is_present[0]
            .definition(String::new())
            .await
            .description,
        "contract"
    );
}

#[test]
fn a_hidden_tool_is_refused_to_the_model_with_the_way_to_call_it() {
    let tools = present_tools(surface(&["script", "query"]), &["query".into()]);
    let refusal = admit(&tools, "query", Origin::Provider).unwrap_err();
    assert_eq!(
        refusal.to_string(),
        "tool 'query' is not offered to direct calls; inspect `tools.query.parameters` in `script`, then call `await tools.query({...})` there"
    );
    assert!(admit(&tools, "query", Origin::Composition).is_ok());
    assert!(admit(&tools, "unknown", Origin::Provider).is_ok());
}

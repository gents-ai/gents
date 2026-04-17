use anyhow::Result;
use defra_agent::graphql::escape_graphql_string;
use defra_agent::ToolSelectionDocument;

use crate::config_writes::ConfigAccess;
use crate::{nullable_string_field, optional_bool_field, optional_string_field, string_list_field};

pub(crate) async fn write_tool_selection_document(
    access: &ConfigAccess,
    selection: &ToolSelectionDocument,
) -> Result<String> {
    let add_fields = tool_selection_fields(selection, true);
    let update_fields = tool_selection_fields(selection, false);
    let mutation = format!(
        r#"mutation {{
            upsert_ToolSelection(
                filter: {{ selection_id: {{ _eq: "{selection_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        selection_id = escape_graphql_string(&selection.selection_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = access.execute(&mutation).await?;
    crate::extract_mutation_doc_id(&response, "ToolSelection")
}

fn tool_selection_fields(selection: &ToolSelectionDocument, include_id: bool) -> String {
    let mut fields = Vec::new();
    if include_id {
        fields.push(format!(
            r#"selection_id: "{}""#,
            escape_graphql_string(&selection.selection_id)
        ));
    }
    fields.push(format!(
        r#"agent_did: "{}""#,
        escape_graphql_string(&selection.agent_did)
    ));
    fields.extend(
        [
            optional_string_field("display_name", selection.display_name.as_deref()),
            optional_bool_field("enable_file_tools", selection.enable_file_tools),
            optional_string_field("file_tools_mode", selection.file_tools_mode.as_deref()),
            Some(nullable_string_field(
                "file_tool_root",
                selection.file_tool_root.as_deref(),
            )),
            optional_bool_field("enable_bash", selection.enable_bash),
            optional_string_field("bash_mode", selection.bash_mode.as_deref()),
            selection
                .cli_tool_names
                .as_ref()
                .and_then(|values| string_list_field("cli_tool_names", values)),
            optional_bool_field("enable_meta_tools", selection.enable_meta_tools),
            selection
                .delegate_to
                .as_ref()
                .and_then(|values| string_list_field("delegate_to", values)),
        ]
        .into_iter()
        .flatten(),
    );
    fields.join(",\n                    ")
}

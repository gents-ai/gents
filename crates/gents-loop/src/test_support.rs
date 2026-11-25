#[cfg(test)]
pub(crate) mod doubles;
pub mod fixed_tool;
pub mod scripted_provider;

#[cfg(test)]
pub(crate) const CORRUPT_TOOL_ARGS_589: &str = "{\"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool房\n</think\": \"\n<tool_call>\n\
     <function=describe_tool>\", \"raw_schema\": false, \
     \"service_id\": \"observability-mcp\", \"tool_name\": \"list_hosts\"}";

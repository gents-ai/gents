const COMMANDS: &[&str] = &["domain_doc_put", "domain_doc_get", "domain_doc_list", "domain_home_path"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}

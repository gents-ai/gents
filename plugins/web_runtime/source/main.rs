fn main() {
    if env!("WEB_TOOL") == "browser_exec" {
        web_runtime::browser::main();
    } else {
        web_runtime::main_for(env!("WEB_TOOL"));
    }
}

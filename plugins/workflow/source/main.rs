//! The `workflow` tool's artifact. The host runs a call's `script` as the step
//! through its embedded JavaScript engine (`script_runtime: js`); this stub
//! answers only a call that carries no script.
use std::io::{Read, Write};

fn main() {
    let mut raw = String::new();
    std::io::stdin().read_to_string(&mut raw).expect("read stdin");
    let out = serde_json::json!({
        "fail": {
            "code": "workflow_invalid_input",
            "message": "pass the workflow as `script`: a JavaScript module whose `module.exports` is the step function `(input) => output`; see the tool description"
        }
    });
    let mut stdout = std::io::stdout();
    serde_json::to_writer(&mut stdout, &out).expect("write stdout");
    stdout.flush().expect("flush stdout");
}

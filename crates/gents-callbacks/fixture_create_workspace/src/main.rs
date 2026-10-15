use std::io::{Read, Write};

fn main() {
    let mut input = Vec::new();
    let result = std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| error.to_string())
        .and_then(|_| gents_callback_fixture_create_workspace::plan_from_bytes(&input));
    match result {
        Ok(output) => std::io::stdout()
            .write_all(&output)
            .expect("write ActionPlan"),
        Err(error) => {
            std::io::stderr()
                .write_all(error.as_bytes())
                .expect("write diagnostic");
            std::process::exit(1);
        }
    }
}

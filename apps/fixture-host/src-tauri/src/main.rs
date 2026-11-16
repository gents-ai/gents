#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let mut domain_home = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--domain-home" {
            let value = args.next().expect("--domain-home requires a path");
            domain_home = Some(std::path::PathBuf::from(value));
        }
    }
    gents_fixture_host_lib::run_with_domain_home(domain_home);
}

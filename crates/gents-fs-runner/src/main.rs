fn main() {
    if let Err(error) = gents_fs_runner::run_stdio_from_args(std::env::args().skip(1)) {
        gents_fs_runner::write_stdio_error(&error);
        std::process::exit(1);
    }
}

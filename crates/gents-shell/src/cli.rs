use crate::keys::KeySet;

pub const USAGE: &str = "usage: gents-shell [--keys vim|emacs]";

pub fn parse(args: &[String]) -> Result<KeySet, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] | ["--keys", "vim"] => Ok(KeySet::Vim),
        ["--keys", "emacs"] => Ok(KeySet::Emacs),
        _ => Err(USAGE.to_string()),
    }
}

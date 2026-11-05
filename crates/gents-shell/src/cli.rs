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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_key_set() {
        assert_eq!(parse(&[]), Ok(KeySet::Vim));
        assert_eq!(parse(&["--keys".into(), "emacs".into()]), Ok(KeySet::Emacs));
        assert_eq!(parse(&["--keys".into(), "vim".into()]), Ok(KeySet::Vim));
    }

    #[test]
    fn rejects_anything_else_with_the_usage() {
        for args in [&["--keys"][..], &["--keys", "nano"], &["vim"]] {
            let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
            assert_eq!(parse(&args), Err(USAGE.to_string()));
        }
    }
}

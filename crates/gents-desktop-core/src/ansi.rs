/// `value` without its terminal escapes: CSI and OSC sequences, and any other escape with the
/// character after it.
pub fn strip_ansi(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            output.push(character);
            continue;
        }
        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                let mut previous_escape = false;
                for next in chars.by_ref() {
                    if next == '\u{7}' || (previous_escape && next == '\\') {
                        break;
                    }
                    previous_escape = next == '\u{1b}';
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn csi_osc_and_other_escapes_are_dropped() {
        let text =
            "\u{1b}[1;31mred\u{1b}[0m \u{1b}]0;title\u{7}a \u{1b}]8;;url\u{1b}\\b\u{1b}Mc\u{1b}";

        assert_eq!(strip_ansi(text), "red a bc");
    }
}

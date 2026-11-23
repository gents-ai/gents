fn short_cluster(arg: &str) -> Option<&str> {
    let letters = arg.strip_prefix('-')?;
    (!letters.is_empty() && !letters.starts_with('-')).then_some(letters)
}

fn long_name(arg: &str) -> Option<&str> {
    let name = arg.strip_prefix("--")?;
    let name = name.split('=').next().unwrap_or(name);
    (!name.is_empty()).then_some(name)
}

pub(super) fn abbreviates_long(arg: &str, option: &str, minimum: usize) -> bool {
    long_name(arg).is_some_and(|name| name.chars().count() >= minimum && option.starts_with(name))
}

pub(super) fn abbreviates_any_long(arg: &str, options: &[&str]) -> bool {
    options
        .iter()
        .any(|option| abbreviates_long(arg, option, 1))
}

pub(super) fn short_cluster_denied(arg: &str, denied: &[char], valued: &[char]) -> bool {
    let Some(letters) = short_cluster(arg) else {
        return false;
    };
    for letter in letters.chars() {
        if denied.contains(&letter) {
            return true;
        }
        if valued.contains(&letter) {
            return false;
        }
    }
    false
}

pub(super) fn cluster_takes_next(arg: &str, valued: &[char]) -> bool {
    let Some(letters) = short_cluster(arg) else {
        return false;
    };
    let count = letters.chars().count();
    for (index, letter) in letters.chars().enumerate() {
        if valued.contains(&letter) {
            return index + 1 == count;
        }
    }
    false
}

pub(super) fn go_flag_name(arg: &str) -> Option<&str> {
    let name = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-'))?;
    Some(name.split('=').next().unwrap_or(name))
}

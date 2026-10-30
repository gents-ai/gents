use rapidhash::RapidHashMap;

const FALLBACK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
const CORE_ENV_VARS: &[&str] = &[
    "PATH",
    "SHELL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "HOME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LOGNAME",
    "USER",
    "RUSTUP_HOME",
    "CARGO_HOME",
    "RUSTUP_TOOLCHAIN",
    "SDKROOT",
    "DEVELOPER_DIR",
];

pub(crate) fn build_shell_env() -> RapidHashMap<String, String> {
    build_shell_env_from_vars(std::env::vars())
}

pub(crate) fn build_shell_env_from_vars<I>(vars: I) -> RapidHashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env = vars
        .into_iter()
        .filter(|(key, _)| {
            CORE_ENV_VARS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(key))
        })
        .filter(|(key, _)| !is_secret_env_name(key))
        .collect::<RapidHashMap<_, _>>();

    env.entry("PATH".to_string())
        .or_insert_with(|| FALLBACK_PATH.to_string());
    env.insert("PAGER".to_string(), "cat".to_string());
    env.insert("GIT_PAGER".to_string(), "cat".to_string());
    env.insert("NO_COLOR".to_string(), "1".to_string());
    env.insert("CLICOLOR".to_string(), "0".to_string());
    env.insert("TERM".to_string(), "dumb".to_string());
    env
}

pub(crate) fn is_secret_env_name(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    key.contains("KEY")
        || key.contains("SECRET")
        || key.contains("TOKEN")
        || key.contains("PASSWORD")
}

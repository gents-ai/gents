use super::*;

/// What the service manager records for the job it holds under the per-user
/// label: the definition file it loaded the job from and the home the job's
/// arguments name. `InstallRepair.ManagerRecord`, read from the manager.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ServiceRecord {
    pub definition: Option<PathBuf>,
    pub home: Option<PathBuf>,
}

/// `InstallRepair.Holder`: whose the job held under the label is, relative to
/// one home and the definition path under the asking process's `HOME`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceHolder {
    Free,
    Chosen,
    Stale,
    Orphan,
    Another {
        home: Option<PathBuf>,
        definition: Option<PathBuf>,
    },
}

impl ServiceHolder {
    /// `InstallRepair.holder`. A record whose home cannot be read is another
    /// home's, taken to exist.
    pub fn of(record: Option<&ServiceRecord>, chosen: &Path, expected: &Path) -> Self {
        let Some(record) = record else {
            return Self::Free;
        };
        let definition = record.definition.as_deref();
        let definition_present = definition.is_some_and(Path::is_file);
        let at_expected = definition.is_some_and(|definition| same_path(definition, expected));
        let another = || Self::Another {
            home: record.home.clone(),
            definition: record.definition.clone(),
        };
        if definition_present && !at_expected {
            return another();
        }
        match record.home.as_deref() {
            Some(home) if same_path(home, chosen) => Self::Chosen,
            Some(home) if !home.exists() && definition_present => Self::Stale,
            Some(home) if !home.exists() => Self::Orphan,
            _ => another(),
        }
    }

    /// `InstallRepair.mayChange`.
    pub fn may_change(&self) -> bool {
        !matches!(self, Self::Another { .. })
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (normalize_absolute(left), normalize_absolute(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// The service manager was asked to change a label whose job serves another
/// home; nothing was changed.
#[derive(Debug, thiserror::Error)]
#[error("the background service {label} runs the node of another home ({}), which this command leaves as it is. Start a node for {home} next to it with `gents runtime server --home {home} --http-port PORT`, or move the background service to it with `gents runtime service repoint --home {home}`", serving_description(.serving, .definition))]
pub struct AnotherHomesService {
    pub label: &'static str,
    pub home: String,
    pub serving: Option<PathBuf>,
    pub definition: Option<PathBuf>,
}

fn serving_description(serving: &Option<PathBuf>, definition: &Option<PathBuf>) -> String {
    match (serving, definition) {
        (Some(home), Some(definition)) => {
            format!("{}, defined at {}", home.display(), definition.display())
        }
        (Some(home), None) => home.display().to_string(),
        (None, Some(definition)) => format!("defined at {}", definition.display()),
        (None, None) => "a home its record does not name".to_owned(),
    }
}

/// External premise: `launchctl print gui/<uid>/<label>` lists the plist a job
/// was loaded from as a `path = ` line and its program arguments one per line
/// inside an `arguments = {` block.
pub(super) fn launchd_record(print: &str) -> ServiceRecord {
    let mut definition = None;
    let mut arguments = None;
    let mut lines = print.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if definition.is_none() {
            if let Some(path) = line.strip_prefix("path = ") {
                definition = Some(PathBuf::from(path));
                continue;
            }
        }
        if arguments.is_none() && line == "arguments = {" {
            arguments = Some(
                lines
                    .by_ref()
                    .take_while(|line| *line != "}")
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            );
        }
    }
    ServiceRecord {
        definition,
        home: arguments.and_then(|arguments| {
            arguments
                .windows(2)
                .find(|pair| pair[0] == "--home")
                .map(|pair| PathBuf::from(&pair[1]))
        }),
    }
}

pub(super) const SYSTEMD_RECORD_PROPERTIES: &str = "--property=LoadState,FragmentPath,ExecStart";

/// External premise: `systemctl --user show --property=LoadState,FragmentPath,ExecStart`
/// prints `key=value` lines; a unit the manager has no file for is
/// `LoadState=not-found`, and `ExecStart` renders the command as
/// `{ path=… ; argv[]=… ; … }`. The fragment, when it is still readable, names
/// the home as the installer wrote it.
pub(super) fn systemd_record(show: &str) -> Option<ServiceRecord> {
    let field = |name: &str| {
        show.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
            .map(str::trim)
            .unwrap_or_default()
    };
    if matches!(field("LoadState"), "" | "not-found") && field("FragmentPath").is_empty() {
        return None;
    }
    let definition = Some(field("FragmentPath"))
        .filter(|path| !path.is_empty())
        .map(PathBuf::from);
    let from_fragment = definition
        .as_deref()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|unit| systemd_home(&unit).ok());
    let from_command = || {
        let exec = field("ExecStart");
        let argv = exec.split_once("argv[]=")?.1;
        let argv = argv.split_once(" ;").map_or(argv, |(argv, _)| argv);
        argv.split_once(" --home ")
            .map(|(_, home)| PathBuf::from(home.trim()))
    };
    Some(ServiceRecord {
        definition,
        home: from_fragment.map(PathBuf::from).or_else(from_command),
    })
}

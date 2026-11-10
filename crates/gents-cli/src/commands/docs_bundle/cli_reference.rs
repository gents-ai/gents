//! The declared clap tree as data. Commands and arguments come from the
//! unbuilt tree, so clap's generated `help` subcommand and `--help` flags and
//! copies of propagated global arguments are not reported as declared surface;
//! usage lines come from the built tree, the only place clap renders them.

use clap::{Arg, Command, CommandFactory};
use serde::Serialize;

use crate::Cli;

#[derive(Debug, Serialize)]
pub(crate) struct CliReference {
    #[serde(flatten)]
    pub(crate) root: CommandReference,
    pub(crate) version: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct CommandReference {
    pub(crate) name: String,
    pub(crate) path: Vec<String>,
    pub(crate) about: Option<String>,
    pub(crate) long_about: Option<String>,
    pub(crate) before_help: Option<String>,
    pub(crate) after_help: Option<String>,
    pub(crate) usage: String,
    pub(crate) aliases: Vec<String>,
    pub(crate) args: Vec<ArgReference>,
    pub(crate) subcommands: Vec<CommandReference>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ArgReference {
    pub(crate) id: String,
    pub(crate) long: Option<String>,
    pub(crate) short: Option<String>,
    pub(crate) value_name: Option<String>,
    pub(crate) help: Option<String>,
    pub(crate) long_help: Option<String>,
    pub(crate) required: bool,
    pub(crate) positional: bool,
    pub(crate) multiple: bool,
    pub(crate) default_values: Vec<String>,
    pub(crate) possible_values: Vec<PossibleValueReference>,
    pub(crate) env: Option<String>,
    pub(crate) global: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PossibleValueReference {
    pub(crate) name: String,
    pub(crate) help: Option<String>,
}

pub(crate) fn cli_reference(version: &str) -> CliReference {
    let declared = Cli::command();
    let mut built = Cli::command();
    built.build();
    let name = declared.get_name().to_owned();
    CliReference {
        root: command_reference(&declared, &mut built, vec![name]),
        version: version.to_owned(),
    }
}

fn command_reference(
    declared: &Command,
    built: &mut Command,
    path: Vec<String>,
) -> CommandReference {
    let subcommands = declared
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .map(|subcommand| {
            let built = built
                .find_subcommand_mut(subcommand.get_name())
                .expect("the built tree holds every declared subcommand");
            let mut path = path.clone();
            path.push(subcommand.get_name().to_owned());
            command_reference(subcommand, built, path)
        })
        .collect();
    CommandReference {
        name: declared.get_name().to_owned(),
        about: declared.get_about().map(ToString::to_string),
        long_about: declared.get_long_about().map(ToString::to_string),
        before_help: declared
            .get_before_long_help()
            .or_else(|| declared.get_before_help())
            .map(ToString::to_string),
        after_help: declared
            .get_after_long_help()
            .or_else(|| declared.get_after_help())
            .map(ToString::to_string),
        usage: usage(built),
        aliases: declared.get_visible_aliases().map(str::to_owned).collect(),
        args: declared
            .get_arguments()
            .filter(|arg| !arg.is_hide_set())
            .map(arg_reference)
            .collect(),
        subcommands,
        path,
    }
}

/// Usage without clap's `Usage:` label and continuation indent.
fn usage(built: &mut Command) -> String {
    let rendered = built.render_usage().to_string();
    let rendered = rendered.strip_prefix("Usage:").unwrap_or(&rendered);
    rendered
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn arg_reference(arg: &Arg) -> ArgReference {
    let takes_values = arg.get_action().takes_values();
    ArgReference {
        id: arg.get_id().to_string(),
        long: arg.get_long().map(str::to_owned),
        short: arg.get_short().map(String::from),
        value_name: takes_values
            .then(|| arg.get_value_names())
            .flatten()
            .map(|names| {
                names
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
        help: arg.get_help().map(ToString::to_string),
        long_help: arg.get_long_help().map(ToString::to_string),
        required: arg.is_required_set(),
        positional: arg.is_positional(),
        multiple: matches!(arg.get_action(), clap::ArgAction::Append)
            || arg
                .get_num_args()
                .is_some_and(|range| range.max_values() > 1),
        default_values: arg
            .get_default_values()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect(),
        possible_values: if takes_values {
            arg.get_possible_values()
                .into_iter()
                .filter(|value| !value.is_hide_set())
                .map(|value| PossibleValueReference {
                    name: value.get_name().to_owned(),
                    help: value.get_help().map(ToString::to_string),
                })
                .collect()
        } else {
            Vec::new()
        },
        env: arg
            .get_env()
            .map(|name| name.to_string_lossy().into_owned()),
        global: arg.is_global_set(),
    }
}

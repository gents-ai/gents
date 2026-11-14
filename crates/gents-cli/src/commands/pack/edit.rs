use std::path::PathBuf;

use anyhow::Result;
pub(super) use gents_pack::pack::authoring::PackEdit;
use gents_pack::pack::authoring::{PackAddition, PackPart as Part};

use crate::cli::{PackAddArgs, PackAddCommand, PackFmtArgs, PackPart, PackRemovePartArgs};

pub(crate) fn fmt(args: PackFmtArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    PackEdit::load(&dir)?.save()
}

pub(crate) fn add(args: PackAddArgs) -> Result<()> {
    let dir = args.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut pack = PackEdit::load(&dir)?;
    apply_add(&mut pack, args.command)?;
    pack.save()
}

pub(super) fn apply_add(pack: &mut PackEdit, command: PackAddCommand) -> Result<()> {
    pack.add(match command {
        PackAddCommand::Agent { id, slot } => PackAddition::Agent { id, slot },
        PackAddCommand::Task { id, agent } => PackAddition::Task { id, agent },
        PackAddCommand::Trigger { id, task, on } => PackAddition::Trigger { id, task, on },
        PackAddCommand::Schema { collection } => PackAddition::Schema { collection },
        PackAddCommand::Skill { id, from } => PackAddition::Skill { id, from },
        PackAddCommand::Plugin {
            name,
            language,
            prebuilt,
        } => PackAddition::Plugin {
            name,
            language,
            prebuilt,
        },
        PackAddCommand::Page { name } => PackAddition::Page { name },
    })
}

pub(crate) fn remove_part(args: PackRemovePartArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let mut pack = PackEdit::load(&dir)?;
    pack.remove_part(
        match args.part {
            PackPart::Agent => Part::Agent,
            PackPart::Task => Part::Task,
            PackPart::Trigger => Part::Trigger,
            PackPart::Plugin => Part::Plugin,
            PackPart::Skill => Part::Skill,
            PackPart::Schema => Part::Schema,
        },
        &args.id,
    )?;
    pack.save()
}

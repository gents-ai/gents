use super::repair_backups::{retained, shared_with, Recorded};
use gents_lean_vocab::families;

#[test]
fn repair_backup_retention_matches_lean() {
    for case in families::repair_backups_retentions::cases() {
        let ids: Vec<u64> = case.input.backups.iter().map(|backup| backup.id).collect();
        let kept: Vec<u64> = retained(&ids, case.input.in_use.as_ref())
            .into_iter()
            .copied()
            .collect();
        case.check(kept);
    }
}

#[test]
fn repair_backup_program_sharing_matches_lean() {
    for case in families::repair_backups_shares::cases() {
        let kept: Vec<Recorded<u64, u64>> = case
            .input
            .kept
            .iter()
            .map(|backup| Recorded {
                id: backup.id,
                program: backup.program,
            })
            .collect();
        case.check(shared_with(&kept, case.input.program.as_ref()).copied());
    }
}

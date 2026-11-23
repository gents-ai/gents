use super::{DestructiveAuthority, InventoryRead};

#[test]
fn destructive_decision_follows_the_model_for_every_available_read() {
    gents_lean_vocab::families::household_destructive_decision::replay(|input| {
        DestructiveAuthority {
            nac_enabled: input.nac_enabled,
            admin: input.admin,
            inventory: InventoryRead::AcpFiltered,
        }
        .decide()
        .err()
        .map(|refusal| refusal.name().to_owned())
    });
}

use super::*;
use gents_lean_vocab::families::account_store_contracts_scenarios as family;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Provider {
    ChatGpt,
    Claude,
    Grok,
    Cloud,
}

const PROVIDERS: [(Provider, &str); 4] = [
    (Provider::ChatGpt, CHATGPT_CODEX_PROVIDER),
    (Provider::Claude, CLAUDE_OAUTH_PROVIDER),
    (Provider::Grok, XAI_OAUTH_PROVIDER),
    (Provider::Cloud, "gents-cloud"),
];

impl Provider {
    fn name(self) -> &'static str {
        PROVIDERS.iter().find(|(known, _)| *known == self).unwrap().1
    }

    fn named(name: &str) -> Self {
        PROVIDERS.iter().find(|(_, known)| *known == name).unwrap().0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepKind {
    SignIn,
    SetLabel,
    SetEnabled,
    Remove,
    LoseBackend,
    RenameBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Added,
    Refreshed,
    Changed,
    Rejected,
}

type Step = family::Step<Provider, StepKind>;
type StepResult = family::StepResult<Outcome>;

fn tokens(provider: Provider, keys: &[String], account_id: Option<&str>) -> (String, Option<String>) {
    match provider {
        Provider::ChatGpt => {
            let mut auth = json!({});
            let mut id = json!({});
            if let Some(account) = account_id {
                auth["chatgpt_account_id"] = json!(account);
                for key in keys {
                    match key.strip_prefix(&format!("{account}:")) {
                        Some(user) => id["chatgpt_user_id"] = json!(user),
                        None => auth["chatgpt_account_user_id"] = json!(key),
                    }
                }
            }
            (
                unsigned_jwt(json!({ "https://api.openai.com/auth": auth })),
                Some(unsigned_jwt(id)),
            )
        }
        Provider::Grok => {
            let claims = match keys.first().and_then(|key| key.split_once(':')) {
                Some((kind, id)) => json!({ "principal_type": kind, "principal_id": id }),
                None => json!({ "sub": "nobody" }),
            };
            (unsigned_jwt(claims), None)
        }
        Provider::Claude | Provider::Cloud => ("access-TEST".into(), None),
    }
}

struct World {
    db: crate::test_support::TestDb,
    access: ConfigAccess,
    slots: Vec<(String, String)>,
}

impl World {
    fn did(&self) -> &str {
        self.db.node_identity.did()
    }

    async fn stored(&self) -> Vec<OAuthCredential> {
        let mut stored = list_oauth_credentials(&self.db.node, self.did())
            .await
            .unwrap();
        stored.sort_by_key(|row| (row.connected_at, self.slot(row)));
        stored
    }

    fn slot(&self, row: &OAuthCredential) -> usize {
        self.slots
            .iter()
            .position(|(doc, _)| Some(doc) == row.doc_id.as_ref())
            .expect("every stored row was seeded or signed in")
    }

    fn credential_id(&self, row: u64) -> &str {
        self.slots
            .get(row as usize)
            .map_or("no-such-account", |(_, id)| id.as_str())
    }

    async fn account_ref(&self, row: u64) -> String {
        let doc = &self.slots[row as usize].0;
        self.stored()
            .await
            .into_iter()
            .find(|stored| stored.doc_id.as_ref() == Some(doc))
            .and_then(|stored| stored.account_ref)
            .expect("an added account")
    }

    async fn seed(&mut self, seed: &family::Seed<Provider>) {
        let index = self.slots.len();
        let did = self.did().to_owned();
        let provider = seed.provider.name();
        let (access_token, id_token) =
            tokens(seed.provider, &seed.token_keys, seed.account_id.as_deref());
        let account_ref = (!seed.original).then(|| format!("seed-{index}"));
        let credential_id = match &account_ref {
            Some(account_ref) => format!("{provider}:{did}:{account_ref}"),
            None => oauth_credential_id(&did, provider),
        };
        seed_raw(
            &self.access,
            json!({
                "credential_id": credential_id,
                "node_did": did,
                "provider": provider,
                "access_token": access_token,
                "refresh_token": seed.token,
                "id_token": id_token,
                "account_id": seed.account_id,
                "is_fedramp": false,
                "access_token_expires_at": "2030-01-01T00:00:00Z",
                "enabled": seed.enabled,
                "provider_account_key": seed.key,
                "label": seed.label,
                "account_ref": account_ref,
                "connected_at": format!("2020-01-01T00:00:{index:02}Z"),
            }),
        )
        .await;
        let doc = list_oauth_credentials(&self.db.node, &did)
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.credential_id == credential_id)
            .and_then(|row| row.doc_id)
            .expect("the seeded row");
        self.slots.push((doc, credential_id));
    }

    async fn sign_in(&mut self, step: &Step) -> StepResult {
        let did = self.did().to_owned();
        let provider = step.provider.name();
        let (access_token, id_token) =
            tokens(step.provider, &step.token_keys, step.account_id.as_deref());
        let credential = OAuthCredential {
            doc_id: None,
            credential_id: oauth_credential_id(&did, provider),
            node_did: did,
            provider: provider.to_owned(),
            access_token,
            refresh_token: step.token.clone(),
            id_token,
            account_id: step.account_id.clone(),
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: Utc::now() + Duration::hours(1),
            last_refresh: None,
            enabled: true,
            account_ref: None,
            connected_at: None,
            provider_account_key: step.key.clone(),
            label: None,
        };
        let Ok(signed) = store_sign_in(&self.access, credential, step.label.as_deref()).await
        else {
            return REJECTED;
        };
        let known = self.slots.iter().position(|(doc, _)| *doc == signed.doc_id);
        let row = known.unwrap_or_else(|| {
            self.slots
                .push((signed.doc_id.clone(), signed.credential.credential_id.clone()));
            self.slots.len() - 1
        });
        StepResult {
            outcome: match signed.result {
                SignInResult::Added => Outcome::Added,
                SignInResult::Refreshed => Outcome::Refreshed,
            },
            row: Some(row as u64),
            identity_matched: signed.identity_matched,
        }
    }

    async fn lose_backend(&self, row: u64) {
        let account_ref = self.account_ref(row).await;
        let provider = self.credential_id(row).split(':').next().unwrap();
        let plan = gents_documents::DesiredStateApplyPlan::new(Vec::new())
            .unwrap()
            .with_removals(vec![(
                gents_store::collection::Collection::InferenceBackend,
                self.did().to_owned(),
                format!("{provider}-{account_ref}"),
            )])
            .unwrap();
        self.access
            .transact_write("test.backend_rm", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &crate::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }

    async fn rename_backend(&self, row: u64, name: &str) {
        let account_ref = self.account_ref(row).await;
        let mut backend = backends(&self.access, self.did())
            .await
            .into_iter()
            .find(|backend| backend.auth.oauth_account_ref() == Some(account_ref.as_str()))
            .expect("the account's backend");
        backend.name = name.to_owned();
        gents_documents::write_inference_backend_document(
            &self.access,
            &backend,
            &crate::resolved::ProviderProfilePolicy,
        )
        .await
        .unwrap();
    }

    async fn step(&mut self, step: &Step) -> StepResult {
        let did = self.did().to_owned();
        let id = self.credential_id(step.row).to_owned();
        let label = step.label.as_deref().unwrap_or_default();
        let done = match step.kind {
            StepKind::SignIn => return self.sign_in(step).await,
            StepKind::SetLabel => set_account_label(&self.access, &did, &id, label)
                .await
                .is_ok(),
            StepKind::SetEnabled => {
                set_account_enabled(&self.access, &did, &id, step.enabled)
                    .await
                    .is_ok()
            }
            StepKind::Remove => remove(&self.access, &did, &id).await.is_ok(),
            StepKind::LoseBackend => {
                self.lose_backend(step.row).await;
                true
            }
            StepKind::RenameBackend => {
                self.rename_backend(step.row, label).await;
                true
            }
        };
        if !done {
            return REJECTED;
        }
        StepResult {
            outcome: Outcome::Changed,
            row: Some(step.row),
            identity_matched: false,
        }
    }

    async fn rows(&self) -> (Vec<family::RowView<Provider>>, u64) {
        let did = self.did();
        let listed = list_accounts(&self.access, did).await.unwrap();
        let all_backends = backends(&self.access, did).await;
        let mut rows = Vec::new();
        for row in self.stored().await {
            let summary = listed
                .iter()
                .find(|account| account.credential_id == row.credential_id);
            let resolved = resolve_oauth_credential(
                &self.access,
                did,
                &row.provider,
                AccountPick::Reference(row.account_ref.as_deref()),
            )
            .await
            .unwrap();
            rows.push(family::RowView {
                id: self.slot(&row) as u64,
                provider: Provider::named(&row.provider),
                effective_label: summary.map_or_else(
                    || effective_account_label(&row),
                    |account| account.label.clone(),
                ),
                original: row.account_ref.is_none(),
                enabled: row.enabled,
                listed: summary.is_some(),
                resolves: resolved.is_some_and(|resolved| resolved.doc_id == row.doc_id),
                backends: all_backends
                    .iter()
                    .filter(|backend| {
                        row.account_ref.is_some()
                            && backend.auth.oauth_account_ref() == row.account_ref.as_deref()
                    })
                    .map(|backend| backend.name.clone())
                    .collect(),
                key: row.provider_account_key,
                account_id: row.account_id,
                label: row.label,
                token: row.refresh_token,
            });
        }
        (rows, all_backends.len() as u64)
    }
}

const REJECTED: StepResult = StepResult {
    outcome: Outcome::Rejected,
    row: None,
    identity_matched: false,
};

#[tokio::test]
async fn generated_account_scenarios_reach_the_modelled_rows() {
    use futures::StreamExt;

    let cases = family::cases::<Outcome, Provider, StepKind>();
    futures::stream::iter(cases)
        .for_each_concurrent(4, |case| async move {
            let (db, _user, access) = access().await;
            let mut world = World {
                db,
                access,
                slots: Vec::new(),
            };
            for seed in &case.input.seeds {
                world.seed(seed).await;
            }
            let mut results = Vec::new();
            for step in &case.input.steps {
                results.push(world.step(step).await);
            }
            let (rows, backend_count) = world.rows().await;
            case.check(family::Output {
                results,
                rows,
                backend_count,
            });
        })
        .await;
}

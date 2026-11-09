use gents_shell::sample::Tone::{Accent, Label, Plain, Warn};
use serde_json::json;

use super::*;

/// An Ask's title and its rows' text, tone and enter act.
fn question(update: Update) -> (String, Vec<(String, Tone, Option<MenuAct>)>) {
    let Update::Ask { title, rows } = update else {
        panic!("{update:?}");
    };
    let rows = rows.into_iter().map(|row| (row.text, row.tone, row.enter));
    (title, rows.collect())
}

fn shown(text: &str, tone: Tone, act: Option<PackAct>) -> (String, Tone, Option<MenuAct>) {
    (text.to_string(), tone, act.map(MenuAct::Pack))
}

fn notice(text: &str) -> Update {
    Update::Notice(text.to_string())
}

#[test]
fn model_call_rows_bind_and_unset() {
    let slots = json!({
        "plugins": [{ "plugin": "acme/ocr", "slot": "vision", "profile": "fast" }],
        "profiles": [
            { "profile_id": "fast", "display_name": "", "model_name": "gpt-fast", "usable": true },
            { "profile_id": "deep", "display_name": "Deep", "model_name": "gpt-deep", "usable": true },
            { "profile_id": "gone", "display_name": "Gone", "model_name": "x", "usable": false },
        ],
    });
    let bind = |profile: Option<&str>| {
        let plugin = "acme/ocr".to_string();
        let profile = profile.map(str::to_string);
        Some(PackAct::BindSlot { plugin, profile })
    };

    let (title, rows) = question(slot_choices("acme/ocr", Ok(&slots)));

    assert_eq!(title, "acme/ocr");
    assert_eq!(
        rows,
        [
            shown(
                " Optional. Unset, the plugin runs without a model.",
                Label,
                None
            ),
            shown(" Not set", Plain, bind(None)),
            shown(" ● gpt-fast", Accent, bind(Some("fast"))),
            shown("   Deep", Plain, bind(Some("deep"))),
        ]
    );
    assert_eq!(slot_choices("acme/ocr", Err("locked")), notice("locked"));
    assert_eq!(quiet(Ok(json!({}))), None, "a bind says nothing once done");
    let refused = quiet(Err("no such profile".to_string()));
    assert_eq!(refused, Some(notice("no such profile")));
}

#[test]
fn a_registry_pack_page_lists_its_detail() {
    let info = json!({
        "namespace": "acme",
        "name": "git",
        "latest": "1.1.0",
        "owner": "acme",
        "homepage": "https://acme.dev/git",
        "plugins": ["git", { "name": "diff" }],
        "versions": [
            { "version": "1.0.0", "published_at": "2026-01-02T12:00:00Z", "size_bytes": 900 },
            { "version": "0.9.0", "yanked": true },
            { "version": "1.1.0", "published_at": "2026-03-05T12:00:00Z", "size_bytes": 2048 },
        ],
    });
    let install = Some(PackAct::Install {
        pack: "acme/git".to_string(),
        edited: EditedChoice::Refuse,
        grant: false,
        slots: Vec::new(),
    });

    let (title, rows) = question(pack_page("acme/git", false, Ok(&info)));
    let (_, installed) = question(pack_page("acme/git", true, Ok(&info)));
    let (_, failed) = question(pack_page("acme/git", false, Err("offline")));

    assert_eq!(title, "acme/git");
    let facts = [
        shown(" by acme", Plain, None),
        shown(" 1.1.0, published Mar 5, 2026", Plain, None),
        shown(" 2.0 KB", Plain, None),
        shown(" 2 plugins: git, diff", Plain, None),
        shown(" 1 older version", Plain, None),
        shown(" Homepage https://acme.dev/git", Plain, None),
    ];
    let mut page = facts.to_vec();
    page.push(shown(" Install", Plain, install.clone()));
    assert_eq!(rows, page);
    assert_eq!(installed, facts);
    let unread = " Couldn't read this pack's page: offline";
    assert_eq!(
        failed,
        [shown(unread, Warn, None), shown(" Install", Plain, install)]
    );

    let local = json!({
        "homepage": "javascript:alert(1)",
        "versions": [{ "version": "0.1.0" }],
    });
    let (_, rows) = question(pack_page("acme/new", true, Ok(&local)));
    assert_eq!(rows, [shown(" 0.1.0", Plain, None)]);
}

/// An installed pack's `.` removes it, a registry pack's `.` opens its page or searches its first
/// keywords, a model call's enter chooses its profile, and the account row's enter signs in and
/// its `.` signs out, as the desktop's labelled button does (`PacksPanel.tsx:417-419`).
#[test]
fn pack_rows_offer_remove_the_pack_page_and_the_account() {
    let installed = json!({ "packs": [
        { "pack": "acme/git", "installed": "1.0.0", "outdated": false, "error": null },
    ]});
    let slots = json!({
        "plugins": [{ "plugin": "acme/ocr", "slot": "vision", "profile": null }],
        "profiles": [],
    });
    let registry = json!({ "packs": [
        { "namespace": "acme", "name": "git", "keywords": [] },
        { "namespace": "acme", "name": "web", "keywords": ["vcs", "diff", "merge"] },
    ], "has_more": false });
    let none = Default::default();
    let read = |account| {
        let (rows, notice) = pack_rows(
            Ok(&installed),
            Ok(&slots),
            account,
            Ok(&registry),
            "",
            1,
            &none,
        );
        assert_eq!(notice, None);
        rows
    };
    let (signed_out, signed_in) = (read(None), read(Some("ada")));
    let named = |text: &str| signed_out.iter().find(|row| row.text == text).expect(text);
    let more = |label: &str, act| (label.to_string(), MenuAct::Pack(act));
    let remove = |pack: &str| {
        let pack = pack.to_string();
        let remove = PackAct::Remove {
            pack,
            confirmed: false,
        };
        more("Remove", remove)
    };
    let about = |pack: &str, installed| {
        let pack = pack.to_string();
        more("About this pack", PackAct::Info { pack, installed })
    };
    let search = |word: &str| more(&format!("Search for {word}"), PackAct::Keyword(word.into()));

    assert_eq!(named(" acme/git   1.0.0").more, [remove("acme/git")]);
    let choose = PackAct::ChooseSlot {
        plugin: "acme/ocr".to_string(),
    };
    assert_eq!(
        named(" acme/ocr   Not set").enter,
        Some(MenuAct::Pack(choose))
    );
    assert_eq!(
        named(" acme/git · Installed").more,
        [about("acme/git", true)]
    );
    assert_eq!(
        named(" acme/web").more,
        [about("acme/web", false), search("vcs"), search("diff")]
    );
    let sign_in = named(" Sign in   A token from your registry dashboard");
    assert_eq!(sign_in.enter, Some(MenuAct::Open(FormRef::PackSignIn)));
    let account = signed_in.last().expect("the account row");
    assert_eq!(account.text, " ada   Signed in to the registry");
    assert_eq!(account.enter, None, "enter does not sign out unsaid");
    assert_eq!(account.more, [more("Sign out", PackAct::SignOut)]);
}

#[test]
fn the_sign_in_form_asks_for_a_masked_token() {
    let form = sign_in_form();

    assert_eq!(form.form, FormRef::PackSignIn);
    assert_eq!(form.title, "Sign in");
    assert_eq!(form.note, ["A token from your registry dashboard"]);
    let token = &form.fields[..];
    let [token] = token else {
        panic!("{token:?}");
    };
    let shown = (&token.key[..], &token.label[..], &token.kind);
    assert_eq!(shown, ("token", "Registry token", &FieldKind::Secret));
    assert_eq!(token.placeholder, "Registry token");
    let signed = signed_in(Ok(json!({})));
    assert_eq!(signed, Ok("Signed in to the registry".to_string()));
    let refused = signed_in(Err("bad token".to_string())).map_err(|error| error.message);
    assert_eq!(refused, Err("bad token".to_string()));
}

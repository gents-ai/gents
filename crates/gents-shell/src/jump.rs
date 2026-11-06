use crate::app::App;
use crate::commands::COMMANDS;
use crate::filter::{rank, Rank};
use crate::sample::{MenuId, MENUS, TOOLS_PACK};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Settings,
    Tabs,
    Sessions,
    Agents,
    Menus,
    Commands,
}

impl Group {
    pub fn header(self) -> &'static str {
        match self {
            Group::Settings => "SETTINGS",
            Group::Tabs => "TABS",
            Group::Sessions => "SESSIONS",
            Group::Agents => "AGENTS",
            Group::Menus => "MENUS",
            Group::Commands => "COMMANDS",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Tab(usize),
    Session(usize),
    /// A position in `App.agents`.
    Agent(usize),
    Menu(MenuId),
    Command(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpItem {
    pub group: Group,
    pub label: String,
    pub meta: String,
    pub target: Target,
    pub rank: Rank,
}

pub fn items(app: &App, query: &str) -> Vec<JumpItem> {
    let mut found = Vec::new();
    let mut add = |group, label: String, meta: &str, target, rank: Option<Rank>| {
        if let Some(rank) = rank {
            let meta = meta.to_string();
            found.push(JumpItem {
                group,
                label,
                meta,
                target,
                rank,
            });
        }
    };
    for (id, meta) in [
        (MenuId::NodeSettings, "this node's configuration"),
        (MenuId::AppSettings, "this computer"),
    ] {
        let name = id.name();
        let matched = rank(query, name, &[]);
        add(
            Group::Settings,
            name.to_string(),
            meta,
            Target::Menu(id),
            matched,
        );
    }
    for (index, tab) in app.tabs.iter().enumerate() {
        let name = format!("{} · {}", tab.name, tab.agent);
        let matched = rank(query, &name, &[]);
        let label = format!("{} {name}", index + 1);
        add(Group::Tabs, label, "", Target::Tab(index), matched);
    }
    for (index, row) in app.session_rows().iter().enumerate() {
        if !row.name.is_empty() {
            let label = format!("○ {}", row.name);
            let matched = rank(query, &row.name, &[]);
            add(
                Group::Sessions,
                label,
                row.date,
                Target::Session(index),
                matched,
            );
        }
    }
    for (index, agent) in app.agents.iter().enumerate() {
        add(
            Group::Agents,
            agent.name.clone(),
            "open in a new tab",
            Target::Agent(index),
            rank(query, &agent.name, &[]),
        );
    }
    for id in MENUS {
        let name = id.name();
        let matched = rank(query, name, &[]);
        add(
            Group::Menus,
            name.to_string(),
            "",
            Target::Menu(id),
            matched,
        );
    }
    let (pack, tools) = TOOLS_PACK;
    add(
        Group::Menus,
        pack.to_string(),
        tools,
        Target::Menu(MenuId::Tools),
        rank(query, pack, &[]),
    );
    for (index, command) in COMMANDS.iter().enumerate() {
        add(
            Group::Commands,
            command.label.to_string(),
            command.about,
            Target::Command(index),
            rank(query, command.label, command.aliases),
        );
    }
    let exact: Vec<Group> = found
        .iter()
        .filter(|item| item.rank == Rank::Exact)
        .map(|item| item.group)
        .collect();
    found.sort_by_key(|item| (!exact.contains(&item.group), item.group, item.rank));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::layers::LayerKind;
    use crate::app::Mode;
    use crate::commands::Effect;
    use crate::draw::theme;
    use crate::keys::KeySet::{self, Emacs, Vim};
    use crate::sample::View;
    use crate::testing::{
        buffer, find, kinds, press, screen, session_row, type_text, with_right_pane,
    };

    const ACCOUNTS: LayerKind = LayerKind::Menu(MenuId::Accounts);

    /// The tabs of the pictures: cli ux, flaky and packs.
    fn with_tabs(set: KeySet, width: u16, height: u16) -> App {
        let mut app = App::new(set, width, height);
        for text in ["cli ux", "flaky", "pack registry"] {
            app.open_session(session_row(text));
        }
        app
    }

    #[test]
    fn new_opens_the_picker_and_agent_the_agents_menu() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "new");
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [LayerKind::Jump, LayerKind::Picker]);
        assert_eq!(app.tabs.len(), 1);

        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "agent");
        press(&mut app, "enter");
        assert_eq!(
            kinds(&app),
            [LayerKind::Jump, LayerKind::Menu(MenuId::Agents)]
        );
    }

    #[test]
    fn rev_groups_like_the_picture() {
        let app = with_tabs(Vim, 132, 36);
        let found = items(&app, "rev");
        let rows: Vec<(Group, &str, &str)> = found
            .iter()
            .map(|item| (item.group, item.label.as_str(), item.meta.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                (Group::Tabs, "2 flaky · Reviewer", ""),
                (Group::Sessions, "○ flaky recovery test", "today"),
                (Group::Sessions, "○ pack review notes", "yesterday"),
                (Group::Agents, "Reviewer", "open in a new tab"),
                (Group::Menus, "Tools > code review pack", "3 tools"),
            ]
        );
        assert_eq!(found[0].target, Target::Tab(1));
        assert_eq!(found[4].target, Target::Menu(MenuId::Tools));
    }

    #[test]
    fn the_bar_lists_settings_first() {
        let app = App::new(Vim, 132, 36);
        let found = items(&app, "");
        assert_eq!(found[0].label, "Node settings");
        assert_eq!(found[0].meta, "this node's configuration");
        assert_eq!(found[0].target, Target::Menu(MenuId::NodeSettings));
        assert_eq!(found[1].label, "App settings");
        assert_eq!(found[1].meta, "this computer");
        assert_eq!(found[1].target, Target::Menu(MenuId::AppSettings));
        assert_eq!(found[0].group, Group::Settings);
        assert_eq!(items(&app, "node set")[0].label, "Node settings");
    }

    #[test]
    fn an_empty_query_lists_every_group_in_order() {
        let found = items(&with_tabs(Vim, 132, 36), "");
        let mut groups: Vec<Group> = found.iter().map(|item| item.group).collect();
        groups.dedup();
        assert_eq!(
            groups,
            [
                Group::Settings,
                Group::Tabs,
                Group::Sessions,
                Group::Agents,
                Group::Menus,
                Group::Commands
            ]
        );
        assert!(found.iter().all(|item| item.rank == Rank::Match));
        let count = |group| found.iter().filter(|item| item.group == group).count();
        assert_eq!(count(Group::Tabs), 3);
        assert_eq!(count(Group::Sessions), 7);
        assert_eq!(count(Group::Agents), 4);
        assert_eq!(count(Group::Menus), 7);
        assert_eq!(count(Group::Commands), COMMANDS.len());
    }

    #[test]
    fn an_exact_match_moves_its_group_first_and_leads_it() {
        let found = items(&with_tabs(Vim, 132, 36), "q");
        assert_eq!(found[0].label, "Quit");
        assert_eq!(found[0].group, Group::Commands);

        let found = items(&with_tabs(Vim, 132, 36), "reviewer");
        assert_eq!(found[0].label, "Reviewer");
        assert_eq!(found[0].group, Group::Agents);
        assert_eq!(found[1].group, Group::Tabs);

        let found = items(&with_tabs(Vim, 132, 36), "tools");
        assert_eq!(found[0].label, "Tools");
        assert_eq!(found[1].label, "Tools > code review pack");
    }

    #[test]
    fn vim_habits_find_quit_through_hidden_aliases() {
        for query in ["q!", "qa", "wq"] {
            let found = items(&with_tabs(Vim, 132, 36), query);
            assert_eq!(found[0].label, "Quit", "{query}");
            assert_eq!(found[0].meta, "quit", "{query}");

            let mut app = App::new(Vim, 132, 36);
            press(&mut app, "esc :");
            type_text(&mut app, query);
            press(&mut app, "enter");
            assert!(app.should_quit(), "{query}");
        }
    }

    #[test]
    fn a_stray_character_does_not_reach_quit() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "!");
        press(&mut app, "enter");
        assert!(!app.should_quit());

        for query in ["!", "a"] {
            let found = items(&with_tabs(Vim, 132, 36), query);
            assert!(found.iter().all(|item| item.label != "Quit"), "{query}");
        }
    }

    #[test]
    fn the_bar_draws_like_the_picture() {
        let mut app = with_tabs(Vim, 132, 36);
        press(&mut app, ":");
        type_text(&mut app, "rev");
        let s = screen(&app);
        assert_eq!(find(&s, " Jump to "), Some((5, 32)));
        assert_eq!(find(&s, " : rev"), Some((6, 31)));
        assert_eq!(find(&s, " TABS"), Some((8, 31)));
        assert_eq!(find(&s, " SESSIONS"), Some((10, 31)));
        assert_eq!(find(&s, " ○ pack review notes          "), Some((12, 31)));
        assert_eq!(find(&s, "yesterday"), Some((12, 70)));
        assert_eq!(find(&s, " AGENTS"), Some((13, 31)));
        assert_eq!(find(&s, "open in a new tab"), Some((14, 70)));
        assert_eq!(find(&s, " MENUS"), Some((15, 31)));
        assert_eq!(find(&s, "3 tools"), Some((16, 70)));
        assert_eq!(find(&s, "COMMANDS"), None);
        let footer = " enter jump · ctrl-n/ctrl-p move · esc back";
        assert_eq!(find(&s, footer), Some((21, 31)));
        assert_eq!(find(&s, "JUMP"), Some((35, 1)));
        let hint = "type to filter tabs, sessions, agents, menus and commands";
        assert_eq!(find(&s, hint), Some((35, 7)));
        let buf = buffer(&app);
        assert_eq!(buf[(32, 9)].symbol(), "2");
        for x in [31, 100] {
            assert_eq!(buf[(x, 9)].bg, theme::SEL_BG, "column {x}");
        }
        assert_ne!(buf[(31, 11)].bg, theme::SEL_BG);

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "alt-x");
        let s = screen(&app);
        assert!(find(&s, "move · ctrl-g back").is_some());
        assert_eq!(find(&s, "esc"), None);
    }

    #[test]
    fn the_bar_under_a_menu_draws_like_the_picture() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "acc");
        press(&mut app, "enter");
        let s = screen(&app);
        assert_eq!(find(&s, " Jump to "), Some((4, 28)));
        assert_eq!(find(&s, " : acc"), Some((5, 27)));
        assert_eq!(find(&s, " Accounts"), Some((8, 27)));
        assert_eq!(find(&s[9..], " Accounts "), Some((1, 58)));
        assert_eq!(find(&s, "enter jump"), None);
        assert!(find(&s, "enter sign in").is_some());
        assert_eq!(find(&s[35..], "MENU"), Some((0, 1)));
    }

    #[test]
    fn q_quits_in_both_sets() {
        for (set, keys) in [(Vim, "esc : q"), (Emacs, "alt-x q"), (Emacs, "ctrl-x x q")] {
            let mut app = App::new(set, 132, 36);
            press(&mut app, keys);
            assert!(!app.should_quit(), "{keys}");
            press(&mut app, "enter");
            assert!(app.should_quit(), "{keys}");
        }
    }

    #[test]
    fn tabnew_finds_new_tab_with_no_slash_command() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "tabnew");
        assert_eq!(items(&app, "tabnew")[0].label, "New tab");
        press(&mut app, "enter");
        assert_eq!(kinds(&app), [LayerKind::Jump, LayerKind::Picker]);
        press(&mut app, "esc");
        assert_eq!(kinds(&app), [LayerKind::Jump]);
        assert!(COMMANDS.iter().all(|c| c.slash != Some("tabnew")));
    }

    #[test]
    fn jumps_close_every_layer() {
        let mut app = with_tabs(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        press(&mut app, ":");
        type_text(&mut app, "flaky");
        press(&mut app, "enter");
        assert!(app.layers.is_empty());
        assert_eq!(app.active, 1);
        assert_eq!(app.tabs.len(), 3);

        let mut app = with_tabs(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        press(&mut app, ":");
        type_text(&mut app, "eval");
        press(&mut app, "enter");
        assert!(app.layers.is_empty());
        assert_eq!(app.active, 3);
        assert_eq!(app.tabs[3].name, "eval");
        assert_eq!(app.tabs[3].agent, "Researcher");

        let mut app = with_tabs(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        press(&mut app, ":");
        type_text(&mut app, "researcher");
        press(&mut app, "enter");
        assert!(app.layers.is_empty());
        assert_eq!(app.active, 3);
        assert_eq!(app.tabs[3].name, "new session");
        assert_eq!(app.tabs[3].agent, "Researcher");
        assert_eq!(app.mode, Mode::Input);
    }

    #[test]
    fn bar_stays_beneath_a_menu_it_opened() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc : a c c enter");
        assert_eq!(kinds(&app), [LayerKind::Jump, ACCOUNTS]);
        press(&mut app, "esc");
        assert_eq!(kinds(&app), [LayerKind::Jump]);
        assert_eq!(app.layers[0].filter.text(), "acc");
        press(&mut app, "esc");
        assert!(app.layers.is_empty());

        let mut app = with_tabs(Vim, 132, 36);
        press(&mut app, ":");
        type_text(&mut app, "tools > code");
        press(&mut app, "enter");
        assert_eq!(
            kinds(&app),
            [LayerKind::Jump, LayerKind::Menu(MenuId::Tools)]
        );
    }

    #[test]
    fn bar_opens_again_from_a_menu() {
        let stack = [LayerKind::Jump, ACCOUNTS, LayerKind::Jump];
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc : a c c enter :");
        assert_eq!(kinds(&app), stack);
        assert_eq!(app.chip(), "JUMP");
        assert!(app.layers[2].filter.is_empty());

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "alt-x a c c enter alt-x");
        assert_eq!(kinds(&app), stack);

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "ctrl-x x a c c enter ctrl-x x");
        assert_eq!(kinds(&app), stack);

        let mut app = App::new(Vim, 60, 16);
        press(&mut app, "esc");
        for _ in 0..19 {
            press(&mut app, ": a c c enter");
            assert!(find(&screen(&app), "sample data").is_some());
        }
        assert_eq!(app.layers.len(), 20);
    }

    #[test]
    fn both_bar_keys_do_nothing_where_text_is_being_typed() {
        for key in ["alt-x", "ctrl-x x"] {
            let mut app = App::new(Emacs, 132, 36);
            press(&mut app, "alt-x");
            press(&mut app, key);
            assert_eq!(kinds(&app), [LayerKind::Jump], "{key}");

            let mut app = App::new(Emacs, 132, 36);
            press(&mut app, "ctrl-x t");
            press(&mut app, key);
            assert_eq!(kinds(&app), [LayerKind::Picker], "{key}");

            let mut app = App::new(Emacs, 132, 36);
            press(&mut app, "ctrl-s");
            press(&mut app, key);
            assert!(app.layers.is_empty(), "{key}");
            assert!(app.search.is_some(), "{key}");
        }
    }

    #[test]
    fn printable_keys_are_filter_text_in_both_sets() {
        for (set, open) in [(Vim, "esc :"), (Emacs, "alt-x")] {
            let mut app = App::new(set, 132, 36);
            press(&mut app, open);
            press(&mut app, "j k");
            assert_eq!(app.layers[0].filter.text(), "jk");
            press(&mut app, "ctrl-c");
            assert!(app.layers[0].filter.is_empty());
            press(&mut app, "ctrl-n down");
            assert_eq!(app.layers[0].selected, 2);
            press(&mut app, "ctrl-p up");
            assert_eq!(app.layers[0].selected, 0);
        }
    }

    #[test]
    fn no_match_enter_is_a_no_op() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "zzzz");
        press(&mut app, "ctrl-n enter");
        assert_eq!(kinds(&app), [LayerKind::Jump]);
        assert!(find(&screen(&app), "no matches").is_some());
    }

    #[test]
    fn long_lists_keep_the_selection_visible() {
        let mut app = App::new(Vim, 60, 16);
        press(&mut app, "esc :");
        assert!(find(&screen(&app), " TABS").is_some());
        for _ in 0..50 {
            press(&mut app, "ctrl-n");
        }
        let last = items(&app, "").len() - 1;
        assert_eq!(app.layers[0].selected, last);
        let s = screen(&app);
        let (row, column) = find(&s, "Sidebar: Background agents").expect("the last item");
        assert!(row < 13, "above the footer");
        assert_eq!(buffer(&app)[(column, row)].bg, theme::SEL_BG);
        assert!(find(&s, "enter jump").is_some());

        press(&mut app, "ctrl-n enter");
        assert_eq!(app.view, View::Agents);
    }

    #[test]
    fn unicode_filter_does_not_panic() {
        let mut app = App::new(Vim, 60, 16);
        press(&mut app, "esc :");
        type_text(&mut app, "日本語");
        assert!(find(&screen(&app), "no matches").is_some());
        press(&mut app, "bs bs");
        assert_eq!(app.layers[0].filter.text(), "日");
        screen(&app);
        type_text(&mut app, &"日".repeat(100));
        screen(&app);
    }

    #[test]
    fn switch_key_set_while_running() {
        let mut app = App::new(Vim, 132, 36);
        press(&mut app, "esc :");
        type_text(&mut app, "switch");
        press(&mut app, "enter");
        assert_eq!(app.keymap.set(), Emacs);
        assert_eq!(app.chip(), "EMACS");
        assert!(app.layers.is_empty());
        press(&mut app, "alt-x");
        assert_eq!(kinds(&app), [LayerKind::Jump]);
        press(&mut app, "esc");

        app.open_session(session_row("flaky"));
        press(&mut app, "alt-x");
        type_text(&mut app, "emacs");
        press(&mut app, "enter");
        assert_eq!(app.keymap.set(), Vim);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.chip(), "NORMAL");

        let mut app = App::new(Emacs, 132, 36);
        press(&mut app, "esc");
        assert_eq!(app.mode, Mode::Normal);
        app.run_effect(Effect::SwitchKeys);
        assert_eq!(app.mode, Mode::Input);
        assert_eq!(app.chip(), "INPUT");
    }

    #[test]
    fn sidebar_views_from_the_bar() {
        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w l ctrl-w l enter :");
        assert_eq!(app.diff, Some(0));
        type_text(&mut app, "work");
        press(&mut app, "enter");
        assert_eq!(app.view, View::Workflows);
        assert_eq!(app.diff, None);
        assert!(app.layers.is_empty());

        let mut app = with_right_pane(App::new(Vim, 132, 36));
        press(&mut app, "esc ctrl-w ] esc :");
        assert!(!app.wanted().sidebar);
        type_text(&mut app, "background");
        press(&mut app, "enter");
        assert_eq!(app.view, View::Agents);
        assert!(app.wanted().sidebar);

        let mut app = with_right_pane(App::new(Vim, 70, 24));
        press(&mut app, "esc :");
        type_text(&mut app, "work");
        press(&mut app, "enter");
        assert_eq!(app.view, View::Workflows);
        assert_eq!(kinds(&app), [LayerKind::Sidebar]);
        assert!(find(&screen(&app), "2 running").is_some());
    }

    #[test]
    fn effects_clear_the_layers_except_those_that_stack() {
        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        app.run_effect(Effect::OpenKeys);
        assert_eq!(kinds(&app), [ACCOUNTS, LayerKind::Keys]);
        app.run_effect(Effect::OpenPicker);
        assert_eq!(kinds(&app), [ACCOUNTS, LayerKind::Keys, LayerKind::Picker]);
        app.run_effect(Effect::NotWired);
        assert!(app.layers.is_empty());
        assert_eq!(app.notice.as_deref(), Some("not wired yet"));

        let mut app = with_tabs(Vim, 132, 36);
        app.tabs[app.active].scroll = 3;
        app.push_layer(ACCOUNTS);
        app.run_effect(Effect::ClearTab);
        assert!(app.layers.is_empty());
        assert!(app.tabs[app.active].lines.is_empty());
        assert_eq!(app.tabs[app.active].scroll, 0);
        assert_eq!(app.mode, Mode::Input);

        let mut app = App::new(Vim, 132, 36);
        app.push_layer(ACCOUNTS);
        app.run_effect(Effect::Quit);
        assert!(app.layers.is_empty());
        assert!(app.should_quit());
    }
}

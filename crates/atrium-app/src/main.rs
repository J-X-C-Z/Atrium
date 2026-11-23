//! Atrium: a native desktop home for coding agents, built on GPUI. Each
//! session works on its own; the atrium is where they share and align.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod agents;
mod alerts;
mod i18n;
use atrium_project as project;
mod docmap;
mod export;
mod links;
mod opener;
mod pageedit;
mod schedule;
mod settings;
mod skills;
mod sleep_inhibitor;
mod ui;
mod updates;
mod workspace;

use ely_gpui_component::{Assets, i18n::I18n};
use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, point, px, size,
};

use ui::{
    AttachFiles, CompactChat, CycleTranscript, FocusComposer, NewChat, OpenPalette, OpenSettings,
    Quit, Reconnect, ReviewChanges, RootView, ShowContext, StopTurn, ToggleChanges, ToggleSidebar,
};

fn main() {
    // `atrium mcp --project <folder> --stream <id>`: the tools project agents
    // call, served over stdio to the agent session that started it.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("mcp") {
        let served = project::mcp::Target::from_args(&args[1..]).and_then(|target| {
            // The device tools of a spread project go through this computer's
            // node, when Atrium said where it is.
            let proxy = workspace::network::Proxy::from_env();
            project::mcp::serve_with(
                &target,
                proxy.as_ref().map(|p| p as &dyn project::mcp::Devices),
                std::io::stdin().lock(),
                std::io::stdout().lock(),
            )
        });
        if let Err(error) = served {
            eprintln!("atrium mcp: {error:#}");
            std::process::exit(2);
        }
        return;
    }
    // `atrium memory-mcp --root <dir> --cwd <dir> --agent <id>`: the shared
    // memory's `recall` and `remember` tools for any agent.
    if args.first().map(String::as_str) == Some("memory-mcp") {
        let served = atrium_memory::mcp::Server::from_args(&args[1..]).and_then(|server| {
            atrium_memory::mcp::serve(&server, std::io::stdin().lock(), std::io::stdout().lock())
        });
        if let Err(error) = served {
            eprintln!("atrium memory-mcp: {error:#}");
            std::process::exit(2);
        }
        return;
    }
    // `atrium skill install`: the skill for Claude Code and Codex;
    // `atrium skill`: print it.
    if args.first().map(String::as_str) == Some("skill") {
        if args.get(1).map(String::as_str) == Some("install") {
            match dirs::home_dir().map(|home| project::skill::install(&home)) {
                Some(Ok(paths)) => paths.iter().for_each(|p| println!("{}", p.display())),
                Some(Err(error)) => {
                    eprintln!("atrium skill: {error:#}");
                    std::process::exit(2);
                }
                None => {
                    eprintln!("atrium skill: no home folder");
                    std::process::exit(2);
                }
            }
        } else {
            print!("{}", project::skill::SKILL);
        }
        return;
    }
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,codex_client=info"),
    )
    .init();

    // `atrium://` links: from the system while running, and from the command line.
    let (links, mut incoming) = futures::channel::mpsc::unbounded::<String>();
    for arg in args.iter().filter(|a| a.starts_with("atrium://")) {
        let _ = links.unbounded_send(arg.clone());
    }
    let app = gpui_kit::application().with_assets(Assets::before(gpui_kit::assets::Assets));
    app.on_open_urls(move |urls| {
        for url in urls {
            let _ = links.unbounded_send(url);
        }
    });
    app.run(move |cx: &mut App| {
        gpui_kit::init(cx);
        ely_gpui_component::init(cx).expect("Ely failed to start");
        apply_language(settings::Settings::load().language, cx);

        cx.bind_keys([
            KeyBinding::new("cmd-n", NewChat, None),
            KeyBinding::new("ctrl-n", NewChat, None),
            KeyBinding::new("cmd-b", ToggleSidebar, None),
            KeyBinding::new("ctrl-b", ToggleSidebar, None),
            KeyBinding::new("cmd-k", OpenPalette, None),
            KeyBinding::new("ctrl-k", OpenPalette, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("cmd-l", FocusComposer, None),
            KeyBinding::new("ctrl-l", FocusComposer, None),
            KeyBinding::new("cmd-shift-a", AttachFiles, None),
            KeyBinding::new("cmd-i", ShowContext, None),
            KeyBinding::new("ctrl-i", ShowContext, None),
            KeyBinding::new("cmd-shift-d", ToggleChanges, None),
            KeyBinding::new("ctrl-o", CycleTranscript, None),
            KeyBinding::new("ctrl-shift-d", ToggleChanges, None),
            KeyBinding::new("cmd-.", StopTurn, None),
            KeyBinding::new("ctrl-.", StopTurn, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("tab", ui::AcceptPrediction, Some("Predicting")),
        ]);
        // After the app-wide keys, so the page editor's own win while it has focus.
        ui::bind_page_keys(cx);
        ui::bind_grid_keys(cx);
        cx.on_action(|_: &Quit, cx| cx.quit());
        ui::register_actions(cx);
        set_menus(cx);

        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Atrium".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(15.))),
            }),
            window_min_size: Some(size(px(640.), px(480.))),
            app_id: Some("dev.atrium".into()),
            ..Default::default()
        };
        let (window, root) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| RootView::new(window, cx))
        })
        .expect("open the main window");
        cx.activate(true);
        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(url) = incoming.next().await {
                let _ = window.update(cx, |_, window, cx| {
                    root.update(cx, |root, cx| root.open_link(&url, window, cx));
                    window.activate_window();
                });
            }
        })
        .detach();
    });
}

/// Shows `language` everywhere: Atrium's strings, Ely's components and the menu bar.
pub fn apply_language(language: i18n::Language, cx: &mut App) {
    let tag = i18n::apply(language);
    if cx.has_global::<I18n>() {
        I18n::set_locale(tag, cx);
    } else {
        cx.set_global(I18n::new(tag).catalog("en-US", &[]).catalog("zh-CN", &[]));
    }
    set_menus(cx);
}

/// The menu bar, in the language shown.
pub fn set_menus(cx: &mut App) {
    use i18n::t;
    let menu = |name: &str, items| Menu {
        name: name.to_string().into(),
        disabled: false,
        items,
    };
    cx.set_menus([
        menu(
            "Atrium",
            vec![
                MenuItem::action(t("Settings…"), OpenSettings),
                MenuItem::separator(),
                MenuItem::action(t("Quit Atrium"), Quit),
            ],
        ),
        menu(
            t("File"),
            vec![
                MenuItem::action(t("New Chat"), NewChat),
                MenuItem::action(t("Attach Files…"), AttachFiles),
                MenuItem::separator(),
                MenuItem::action(t("Restart Codex"), Reconnect),
            ],
        ),
        menu(
            t("Chat"),
            vec![
                MenuItem::action(t("Command Palette"), OpenPalette),
                MenuItem::action(t("Focus Composer"), FocusComposer),
                MenuItem::action(t("Stop Answer"), StopTurn),
                MenuItem::action(t("Context & Speed"), ShowContext),
                MenuItem::action(t("Changes"), ToggleChanges),
                MenuItem::action(t("Cycle Normal / Thinking / Verbose"), CycleTranscript),
                MenuItem::separator(),
                MenuItem::action(t("Review Uncommitted Changes"), ReviewChanges),
                MenuItem::action(t("Compact Conversation"), CompactChat),
            ],
        ),
        menu(
            t("View"),
            vec![MenuItem::action(t("Toggle Sidebar"), ToggleSidebar)],
        ),
    ]);
}

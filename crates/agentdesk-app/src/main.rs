//! AgentDesk: a native desktop front end for coding agents, built on GPUI.
//! This first cut drives the Codex CLI (`codex app-server`).

mod settings;
mod ui;
mod workspace;

use ely_gpui_component::Assets;
use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, TitlebarOptions, WindowBounds,
    WindowOptions, point, px, size,
};

use ui::{
    AttachFiles, CompactChat, FocusComposer, NewChat, OpenPalette, OpenSettings, Quit, Reconnect,
    ReviewChanges, RootView, StopTurn, ToggleSidebar,
};

fn main() {
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,codex_client=info"),
    )
    .init();

    gpui_kit::application()
        .with_assets(Assets::before(gpui_kit::assets::Assets))
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            ely_gpui_component::init(cx).expect("Ely failed to start");

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
                KeyBinding::new("cmd-.", StopTurn, None),
                KeyBinding::new("ctrl-.", StopTurn, None),
                KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            ui::register_actions(cx);
            let menu = |name: &str, items| Menu {
                name: name.to_string().into(),
                disabled: false,
                items,
            };
            cx.set_menus([
                menu(
                    "AgentDesk",
                    vec![
                        MenuItem::action("Settings…", OpenSettings),
                        MenuItem::separator(),
                        MenuItem::action("Quit AgentDesk", Quit),
                    ],
                ),
                menu(
                    "File",
                    vec![
                        MenuItem::action("New Chat", NewChat),
                        MenuItem::action("Attach Files…", AttachFiles),
                        MenuItem::separator(),
                        MenuItem::action("Restart Codex", Reconnect),
                    ],
                ),
                menu(
                    "Chat",
                    vec![
                        MenuItem::action("Command Palette", OpenPalette),
                        MenuItem::action("Focus Composer", FocusComposer),
                        MenuItem::action("Stop Answer", StopTurn),
                        MenuItem::separator(),
                        MenuItem::action("Review Uncommitted Changes", ReviewChanges),
                        MenuItem::action("Compact Conversation", CompactChat),
                    ],
                ),
                menu(
                    "View",
                    vec![MenuItem::action("Toggle Sidebar", ToggleSidebar)],
                ),
            ]);

            let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("AgentDesk".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(14.), px(15.))),
                }),
                window_min_size: Some(size(px(640.), px(480.))),
                app_id: Some("dev.agentdesk".into()),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| RootView::new(window, cx))
            })
            .expect("open the main window");
            cx.activate(true);
        });
}

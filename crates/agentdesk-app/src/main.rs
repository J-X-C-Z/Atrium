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

use ui::{NewChat, Quit, Reconnect, RootView, StopTurn, ToggleSidebar};

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
                KeyBinding::new("cmd-.", StopTurn, None),
                KeyBinding::new("cmd-q", Quit, None),
            ]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus([
                Menu {
                    name: "AgentDesk".into(),
                    disabled: false,
                    items: vec![MenuItem::action("Quit AgentDesk", Quit)],
                },
                Menu {
                    name: "File".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("New Chat", NewChat),
                        MenuItem::action("Restart Codex", Reconnect),
                    ],
                },
                Menu {
                    name: "View".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("Toggle Sidebar", ToggleSidebar),
                        MenuItem::action("Stop Answer", StopTurn),
                    ],
                },
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

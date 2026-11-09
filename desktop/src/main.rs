mod app;
mod protocol;
mod runner;
mod settings;
mod text_input;

use gpui::{App, Application, Bounds, Focusable, WindowBounds, WindowOptions, prelude::*, px, size};

fn main() {
    Application::new().run(|cx: &mut App| {
        text_input::bind_keys(cx);
        app::register(cx);

        let bounds = Bounds::centered(None, size(px(1080.), px(760.)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("pi-docs".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(app::AppView::new),
            )
            .expect("failed to open the main window");

        let view = window.update(cx, |_, _, cx| cx.entity()).expect("window is open");
        let closing = view.clone();
        cx.on_window_closed(move |cx| {
            closing.update(cx, |view, _| view.shutdown());
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle(cx));
                cx.activate(true);
            })
            .ok();
    });
}

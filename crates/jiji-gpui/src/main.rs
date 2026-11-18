//! Jiji: a jj-native desktop workbench, on GPUI.
//!
//! With arguments this binary is the `jiji` command line (the installed
//! launcher forwards to it); without, it is the app.

mod app;
mod diff;
mod headless;
mod host;
mod logic;
mod script;
mod shell;
mod store;
#[cfg(test)]
mod testing;
mod theme;
mod ui;
mod views;

use gpui::{
    point, prelude::*, px, size, App, Bounds, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions,
};

use crate::app::AppModel;
use crate::shell::Shell;
use crate::store::Store;

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        // GPUI and its dependencies are chatty at info; keep theirs to
        // warnings.
        let ours = record.target().starts_with("jiji");
        if self.enabled(record.metadata()) && (ours || record.level() <= log::Level::Warn) {
            eprintln!(
                "[{}] {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// SF Mono when it is installed (it ships with Xcode and Terminal, not
/// with the system), else Menlo, which every Mac has.
fn mono_font(cx: &App) -> SharedString {
    let names = cx.text_system().all_font_names();
    for candidate in ["SF Mono", "SFMono-Regular"] {
        if names.iter().any(|name| name == candidate) {
            return candidate.into();
        }
    }
    "Menlo".into()
}

fn main() {
    if std::env::args_os().nth(1).is_some() {
        std::process::exit(jiji_cli::run(std::env::args_os()));
    }
    let _ = log::set_logger(&Logger).map(|()| log::set_max_level(log::LevelFilter::Info));

    // Development switches ride the environment so argv stays the CLI's:
    //   JIJI_OPEN=<path>      open this repository at launch; several,
    //                         separated like PATH, arrive together the
    //                         way folders opened from Finder do
    //   JIJI_SCRIPT=<steps>   drive the window (see script.rs)
    //   JIJI_HEADLESS=1       never show the window (see headless.rs)
    let open_at_launch: Vec<std::path::PathBuf> = std::env::var_os("JIJI_OPEN")
        .map(|paths| {
            std::env::split_paths(&paths)
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    let refuse = |why: String| -> ! {
        eprintln!("jiji: {why}");
        std::process::exit(2);
    };
    let script = match std::env::var("JIJI_SCRIPT").ok().map(|s| script::parse(&s)) {
        Some(Ok(steps)) => steps,
        Some(Err(err)) => refuse(err),
        None => Vec::new(),
    };
    let headless = std::env::var("JIJI_HEADLESS").as_deref() == Ok("1");
    if std::env::var_os("JIJI_BACKGROUND").is_some() {
        refuse("JIJI_BACKGROUND is gone: use JIJI_HEADLESS=1, which shows no window at all".into());
    }
    if headless && script.is_empty() {
        refuse("JIJI_HEADLESS needs a JIJI_SCRIPT: nothing else can reach a window that is never shown".into());
    }
    if !headless::SUPPORTED {
        if headless {
            refuse(format!("this build cannot run headless: {}", headless::HOW));
        }
        if script
            .iter()
            .any(|step| matches!(step, script::Step::Shot(_)))
        {
            refuse(format!(
                "this build cannot take screenshots: {}",
                headless::HOW
            ));
        }
    }

    let application = gpui_platform::application().with_assets(ui::icon::Assets);
    // No Dock icon and no menu bar for a run nobody is meant to see.
    let application = if headless {
        application.with_windowing(gpui::WindowingRequest::Headless)
    } else {
        application
    };
    // Folders arriving through Launch Services — the `jiji` launcher's
    // `open -b`, or a drop on the Dock icon.
    application.on_open_urls(|urls| {
        let paths: Vec<std::path::PathBuf> = urls
            .iter()
            .filter_map(|url| url.strip_prefix("file://"))
            .map(|path| std::path::PathBuf::from(percent_decode(path)))
            .collect();
        if !paths.is_empty() {
            PENDING_OPEN.lock().unwrap().extend(paths);
        }
    });
    application.run(move |cx: &mut App| {
        ui::tooltip::init(cx);
        ui::input::bind_keys(cx);
        shell::bind_keys(cx);
        views::workbench::bind_keys(cx);
        views::bind_keys(cx);
        views::palette::bind_keys(cx);
        views::publish::bind_keys(cx);
        views::search::bind_keys(cx);
        diff::view::bind_keys(cx);
        shell::register_app_actions(cx);
        cx.set_menus(shell::menus());

        let store = Store::open();
        // A headless run always starts at the default size: its
        // screenshots should not depend on where a window was last left.
        let geometry = store.window().filter(|_| !headless);
        let model = AppModel::install(store, mono_font(cx), cx);
        if !open_at_launch.is_empty() {
            model.update(cx, |model, cx| model.open_paths(open_at_launch, cx));
        }

        let default_size = size(px(1280.), px(820.));
        let bounds = match geometry {
            Some(saved) if saved.width >= 400. && saved.height >= 300. => Bounds::new(
                point(px(saved.x), px(saved.y)),
                size(px(saved.width), px(saved.height)),
            ),
            _ => Bounds::centered(None, default_size, cx),
        };
        let fullscreen = geometry.is_some_and(|saved| saved.fullscreen);
        let options = WindowOptions {
            window_bounds: Some(if fullscreen {
                WindowBounds::Fullscreen(bounds)
            } else {
                WindowBounds::Windowed(bounds)
            }),
            window_min_size: Some(size(px(980.), px(640.))),
            titlebar: Some(TitlebarOptions {
                title: Some("Jiji".into()),
                appears_transparent: true,
                // Centers the traffic lights on the tab band (the title
                // bar below its top six points).
                traffic_light_position: Some(point(px(14.), px(14.))),
            }),
            focus: !headless,
            show: !headless,
            ..Default::default()
        };
        let window = cx
            .open_window(options, |window, cx| cx.new(|cx| Shell::new(window, cx)))
            .expect("the main window opens");
        if headless {
            headless::pump_frames(window.into(), cx);
        } else {
            cx.activate(true);
        }
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        // Drain folders handed over before (and after) the window existed.
        let weak = model.downgrade();
        cx.spawn(async move |cx| loop {
            let pending: Vec<_> = PENDING_OPEN.lock().unwrap().drain(..).collect();
            if !pending.is_empty() {
                let delivered = weak.update(cx, |model, cx| model.open_paths(pending, cx));
                if delivered.is_err() {
                    break;
                }
                if !headless {
                    cx.update(|cx| cx.activate(true));
                }
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(150))
                .await;
        })
        .detach();

        if !script.is_empty() {
            script::run(script, window.into(), headless, cx);
        }
    });
}

/// Paths the OS asked this app to open; drained on the main thread.
static PENDING_OPEN: std::sync::Mutex<Vec<std::path::PathBuf>> = std::sync::Mutex::new(Vec::new());

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        if bytes[ix] == b'%' && ix + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&text[ix + 1..ix + 3], 16) {
                out.push(byte);
                ix += 3;
                continue;
            }
        }
        out.push(bytes[ix]);
        ix += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_decode_to_paths() {
        assert_eq!(percent_decode("/Users/me/My%20Repo"), "/Users/me/My Repo");
        assert_eq!(percent_decode("/plain/path"), "/plain/path");
        assert_eq!(percent_decode("/odd%"), "/odd%");
        assert_eq!(percent_decode("/caf%C3%A9"), "/café");
    }
}

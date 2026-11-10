//! Spike: Jiji's diff view on GPUI instead of the webview.
//!
//! Loads one change's diff (or a comparison between two changes) straight
//! from `jiji-core` — no IPC, no serialization — and renders it with
//! windowing, unified and split layouts, syntax highlighting, selection
//! and copy. See README.md for what this is meant to answer.

mod capture;
mod highlight;
mod model;
mod theme;
mod view;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use gpui::{
    point, prelude::*, px, size, App, Application, Bounds, Focusable, KeyBinding, TitlebarOptions,
    WindowBounds, WindowOptions,
};
use jiji_core::snapshot::ChangeDiff;
use jiji_core::{JjBackend, RepoBackend};

use crate::model::DiffDoc;
use crate::view::{BenchPlan, DiffView, Options, Step};

const USAGE: &str = "\
Usage: jiji-gpui [REPO] [OPTIONS]

Renders one change's diff from a jj repository in a GPUI window.

Arguments:
  [REPO]             Workspace root of a jj repository [default: .]

Options:
  -c, --change <ID>  Change to show: a change-id prefix or a commit id
                     [default: the working copy]
      --from <ID>    Compare from this change's tree instead of the parent
      --split        Start in the side-by-side layout
      --repeat <N>   Repeat the diff's files N times (stress the windowing)
      --bench        Sweep the diff at 9000 and 18000 px/s and print frame
                     timings; add --exit to quit when done
      --background   Do not take focus on launch (for scripted runs); the
                     window floats above others so it keeps painting
      --script <OPS> Drive the view without the pointer, then leave it open.
                     Steps separated by ';', coordinates in window points:
                     scroll=Y  pan=X  split  unified  drag=X0,Y0,X1,Y1
                     click=X,Y  double=X,Y  triple=X,Y  key=KEYSTROKE
                     print (what cmd-c would copy)  shot=PATH (window to
                     PNG)  quit
  -h, --help         Print help

Keys: s layout · ]/[ next/previous file · space/shift-space page ·
      cmd-up/down ends · cmd-a select all · cmd-c copy · b benchmark ·
      f frame stats";

struct Args {
    repo: PathBuf,
    change: Option<String>,
    from: Option<String>,
    split: bool,
    repeat: usize,
    bench: bool,
    exit: bool,
    background: bool,
    script: Vec<Step>,
}

fn parse_script(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for op in script.split(';').map(str::trim).filter(|op| !op.is_empty()) {
        let (name, value) = op.split_once('=').unwrap_or((op, ""));
        match name {
            "shot" if !value.is_empty() => {
                steps.push(Step::Shot(PathBuf::from(value)));
                continue;
            }
            "key" if !value.is_empty() => {
                steps.push(Step::Key(value.to_owned()));
                continue;
            }
            _ => {}
        }
        let numbers: Vec<f32> = value
            .split(',')
            .filter(|part| !part.is_empty())
            .map(|part| {
                part.trim()
                    .parse()
                    .map_err(|_| format!("bad number in `{op}`"))
            })
            .collect::<Result<_, _>>()?;
        steps.push(match (name, numbers.as_slice()) {
            ("scroll", [y]) => Step::Scroll(*y),
            ("pan", [x]) => Step::Pan(*x),
            ("split", []) => Step::Split(true),
            ("unified", []) => Step::Split(false),
            ("drag", [x0, y0, x1, y1]) => Step::Drag(*x0, *y0, *x1, *y1),
            ("click", [x, y]) => Step::Click(1, *x, *y),
            ("double", [x, y]) => Step::Click(2, *x, *y),
            ("triple", [x, y]) => Step::Click(3, *x, *y),
            ("print", []) => Step::Print,
            ("quit", []) => Step::Quit,
            _ => return Err(format!("unknown script step `{op}`")),
        });
    }
    Ok(steps)
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        repo: PathBuf::from("."),
        change: None,
        from: None,
        split: false,
        repeat: 1,
        bench: false,
        exit: false,
        background: false,
        script: Vec::new(),
    };
    let mut input = std::env::args().skip(1);
    while let Some(arg) = input.next() {
        let mut value = |name: &str| input.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => return Err(String::new()),
            "-c" | "--change" => args.change = Some(value("--change")?),
            "--from" => args.from = Some(value("--from")?),
            "--split" => args.split = true,
            "--repeat" => {
                args.repeat = value("--repeat")?
                    .parse()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or("--repeat needs a positive number")?
            }
            "--bench" => args.bench = true,
            "--exit" => args.exit = true,
            "--background" => args.background = true,
            "--script" => args.script = parse_script(&value("--script")?)?,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            path => args.repo = PathBuf::from(path),
        }
    }
    Ok(args)
}

fn load(args: &Args) -> Result<(ChangeDiff, String), String> {
    let backend = JjBackend::default();
    let repo = args
        .repo
        .canonicalize()
        .map_err(|err| format!("{}: {err}", args.repo.display()))?;
    let change = match &args.change {
        Some(change) => change.clone(),
        None => {
            backend
                .open(&repo)
                .map_err(|err| err.to_string())?
                .working_copy
        }
    };
    let diff = match &args.from {
        Some(from) => backend.compare_diff(&repo, from, &change),
        None => backend.change_diff(&repo, &change),
    }
    .map_err(|err| err.to_string())?;
    let short = |id: &str| id.chars().take(8).collect::<String>();
    let name = repo
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let title = match &args.from {
        Some(from) => format!("{name} · {} → {}", short(from), short(&change)),
        None => format!("{name} · {}", short(&change)),
    };
    Ok((diff, title))
}

fn main() -> ExitCode {
    let launched = Instant::now();
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) if message.is_empty() => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("jiji-gpui: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let started = Instant::now();
    let mut args = args;
    let (mut diff, title) = match load(&args) {
        Ok(loaded) => loaded,
        Err(message) => {
            eprintln!("jiji-gpui: {message}");
            return ExitCode::FAILURE;
        }
    };
    let loaded = started.elapsed();
    if args.repeat > 1 {
        let files = diff.files.clone();
        for _ in 1..args.repeat {
            diff.files.extend(files.iter().cloned());
        }
    }
    let built = Instant::now();
    let doc = DiffDoc::build(&diff);
    drop(diff);
    eprintln!(
        "jiji-gpui: {} files, {} lines (+{} −{}); diff loaded in {:.0?}, rows built in {:.1?} \
         ({} unified, {} split)",
        doc.files.len(),
        doc.lines.len(),
        doc.added,
        doc.removed,
        loaded,
        built.elapsed(),
        doc.unified.rows.len(),
        doc.split.rows.len(),
    );

    let options = Options {
        split: args.split,
        bench: args.bench.then(|| BenchPlan {
            speeds: vec![9_000., 18_000.],
            exit: args.exit,
        }),
        script: std::mem::take(&mut args.script),
        launched,
    };
    let background = args.background;

    Application::new().run(move |cx: &mut App| {
        let context = Some("DiffView");
        cx.bind_keys([
            KeyBinding::new("cmd-c", view::CopySelection, context),
            KeyBinding::new("cmd-a", view::SelectAll, context),
            KeyBinding::new("s", view::ToggleLayout, context),
            KeyBinding::new("space", view::PageDown, context),
            KeyBinding::new("pagedown", view::PageDown, context),
            KeyBinding::new("shift-space", view::PageUp, context),
            KeyBinding::new("pageup", view::PageUp, context),
            KeyBinding::new("cmd-up", view::ScrollTop, context),
            KeyBinding::new("home", view::ScrollTop, context),
            KeyBinding::new("cmd-down", view::ScrollBottom, context),
            KeyBinding::new("end", view::ScrollBottom, context),
            KeyBinding::new("]", view::NextFile, context),
            KeyBinding::new("[", view::PrevFile, context),
            KeyBinding::new("b", view::RunBench, context),
            KeyBinding::new("f", view::ToggleHud, context),
            KeyBinding::new("cmd-q", view::Quit, None),
            KeyBinding::new("cmd-w", view::Quit, None),
        ]);
        cx.on_action(|_: &view::Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Jiji · GPUI diff spike".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(13.), px(12.))),
                }),
                focus: !background,
                window_min_size: Some(size(px(980.), px(640.))),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| DiffView::new(doc, title.into(), options, window, cx));
                window.focus(&view.focus_handle(cx));
                if background {
                    capture::float_without_focus(window);
                }
                view
            },
        )
        .expect("the window opens");
        if !background {
            cx.activate(true);
        }
    });
    ExitCode::SUCCESS
}

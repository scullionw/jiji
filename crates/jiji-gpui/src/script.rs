//! Scripted runs: drive the real window without the pointer or keyboard.
//!
//! Steps are injected through GPUI's own event dispatch, so they take the
//! same path OS input does (hit-testing, focus, key bindings, the input
//! method) — but nothing is synthesized at the OS level, the window never
//! takes focus, and the clipboard is only touched by a step that copies.
//!
//! `JIJI_SCRIPT` holds the steps, separated by `;`, coordinates in window
//! points:
//!
//!   click=X,Y   dclick=X,Y   tclick=X,Y   rclick=X,Y   mclick=X,Y
//!   shift-click=X,Y   alt-click=X,Y   cmd-click=X,Y
//!   move=X,Y    drag=X0,Y0,X1,Y1   alt-drag=…
//!   press=X,Y   hold=X,Y   release=X,Y   (a drag in parts; alt-hold=…)
//!   scroll=X,Y,DX,DY               key=cmd-k   type=some text
//!   wait=MS     shot=/abs/path.png resize=W,H   quit

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    px, size, AnyWindowHandle, App, AppContext as _, AsyncApp, Keystroke, Modifiers, MouseButton,
    Window,
};

use crate::capture::{NativeTarget, Pointer};

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Click {
        at: (f32, f32),
        count: usize,
        button: MouseButton,
        modifiers: Modifiers,
    },
    Move(f32, f32),
    Drag {
        from: (f32, f32),
        to: (f32, f32),
        modifiers: Modifiers,
    },
    /// The halves of a drag, for looking at one while it is held.
    Press((f32, f32), Modifiers),
    Hold((f32, f32), Modifiers),
    Release((f32, f32), Modifiers),
    Scroll {
        at: (f32, f32),
        delta: (f32, f32),
    },
    Key(String),
    Type(String),
    Wait(u64),
    Shot(PathBuf),
    Resize(f32, f32),
    Quit,
}

impl Step {
    /// Where the step leaves the scripted pointer, if it moves it.
    fn pointer_after(&self) -> Option<(f32, f32)> {
        match self {
            Step::Click { at, .. } | Step::Scroll { at, .. } => Some(*at),
            Step::Move(x, y) => Some((*x, *y)),
            Step::Drag { to, .. } => Some(*to),
            Step::Press(at, _) | Step::Hold(at, _) | Step::Release(at, _) => Some(*at),
            Step::Key(_)
            | Step::Type(_)
            | Step::Wait(_)
            | Step::Shot(_)
            | Step::Resize(..)
            | Step::Quit => None,
        }
    }
}

fn numbers<const N: usize>(op: &str, value: &str) -> Result<[f32; N], String> {
    let parsed: Result<Vec<f32>, _> = value.split(',').map(|n| n.trim().parse()).collect();
    parsed
        .ok()
        .and_then(|values| values.try_into().ok())
        .ok_or_else(|| format!("script step `{op}` expects {N} numbers"))
}

pub fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for op in script.split(';').map(str::trim).filter(|op| !op.is_empty()) {
        let (name, value) = op.split_once('=').unwrap_or((op, ""));
        // Text-valued steps first: their values may hold commas.
        match name {
            "type" => {
                steps.push(Step::Type(value.to_owned()));
                continue;
            }
            "key" => {
                steps.push(Step::Key(value.to_owned()));
                continue;
            }
            "shot" => {
                steps.push(Step::Shot(PathBuf::from(value)));
                continue;
            }
            "quit" => {
                steps.push(Step::Quit);
                continue;
            }
            "wait" => {
                let ms = value
                    .parse()
                    .map_err(|_| format!("script step `{op}` expects milliseconds"))?;
                steps.push(Step::Wait(ms));
                continue;
            }
            _ => {}
        }
        let mut modifiers = Modifiers::default();
        let mut base = name;
        loop {
            if let Some(rest) = base.strip_prefix("shift-") {
                modifiers.shift = true;
                base = rest;
            } else if let Some(rest) = base.strip_prefix("alt-") {
                modifiers.alt = true;
                base = rest;
            } else if let Some(rest) = base.strip_prefix("cmd-") {
                modifiers.platform = true;
                base = rest;
            } else {
                break;
            }
        }
        let click = |count: usize, button: MouseButton| -> Result<Step, String> {
            let [x, y] = numbers(op, value)?;
            Ok(Step::Click {
                at: (x, y),
                count,
                button,
                modifiers,
            })
        };
        steps.push(match base {
            "click" => click(1, MouseButton::Left)?,
            "dclick" => click(2, MouseButton::Left)?,
            "tclick" => click(3, MouseButton::Left)?,
            "rclick" => click(1, MouseButton::Right)?,
            "mclick" => click(1, MouseButton::Middle)?,
            "move" => {
                let [x, y] = numbers(op, value)?;
                Step::Move(x, y)
            }
            "press" => {
                let [x, y] = numbers(op, value)?;
                Step::Press((x, y), modifiers)
            }
            "hold" => {
                let [x, y] = numbers(op, value)?;
                Step::Hold((x, y), modifiers)
            }
            "release" => {
                let [x, y] = numbers(op, value)?;
                Step::Release((x, y), modifiers)
            }
            "drag" => {
                let [x0, y0, x1, y1] = numbers(op, value)?;
                Step::Drag {
                    from: (x0, y0),
                    to: (x1, y1),
                    modifiers,
                }
            }
            "scroll" => {
                let [x, y, dx, dy] = numbers(op, value)?;
                Step::Scroll {
                    at: (x, y),
                    delta: (dx, dy),
                }
            }
            "resize" => {
                let [w, h] = numbers(op, value)?;
                Step::Resize(w, h)
            }
            _ => return Err(format!("unknown script step `{op}`")),
        });
    }
    Ok(steps)
}

/// A beat for the frame an event caused to paint: the next event
/// hit-tests against it.
async fn frame(cx: &AsyncApp) {
    cx.background_executor()
        .timer(Duration::from_millis(60))
        .await;
}

async fn run_step(step: Step, window: AnyWindowHandle, cx: &mut AsyncApp) {
    let update = |cx: &mut AsyncApp, f: &mut dyn FnMut(&mut Window, &mut App)| {
        let _ = cx.update_window(window, |_, window, cx| f(window, cx));
    };
    // Read the native handles while GPUI lends the window out, then
    // deliver with the app unborrowed: the view's handler re-enters it.
    let target = |cx: &mut AsyncApp| {
        cx.update_window(window, |_, window, _| NativeTarget::of(window))
            .map_err(|err| err.to_string())
            .and_then(|target| target)
    };
    let mouse = |cx: &mut AsyncApp,
                 kind: Pointer,
                 button: MouseButton,
                 position: (f32, f32),
                 clicks: usize,
                 modifiers: Modifiers| {
        let sent = target(cx)
            .and_then(|target| target.send_mouse(kind, button, position, clicks, modifiers));
        if let Err(err) = sent {
            eprintln!("jiji: pointer step failed: {err}");
        }
    };
    match step {
        Step::Click {
            at: position,
            count,
            button,
            modifiers,
        } => {
            mouse(cx, Pointer::Move, button, position, 0, modifiers);
            frame(cx).await;
            // A double click is two presses, the second counted twice —
            // what the platform sends.
            for n in 1..=count {
                mouse(cx, Pointer::Down, button, position, n, modifiers);
                frame(cx).await;
                mouse(cx, Pointer::Up, button, position, n, modifiers);
                frame(cx).await;
            }
        }
        Step::Move(x, y) => {
            mouse(
                cx,
                Pointer::Move,
                MouseButton::Left,
                (x, y),
                0,
                Modifiers::default(),
            );
        }
        Step::Drag {
            from,
            to,
            modifiers,
        } => {
            let button = MouseButton::Left;
            mouse(cx, Pointer::Move, button, from, 0, modifiers);
            frame(cx).await;
            mouse(cx, Pointer::Down, button, from, 1, modifiers);
            frame(cx).await;
            // Travel in a few hops: the first leaves the drag threshold,
            // the rest let drop targets react on the way.
            for step in 1..=4 {
                let t = step as f32 / 4.;
                let hop = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
                mouse(cx, Pointer::Drag, button, hop, 1, modifiers);
                frame(cx).await;
            }
            frame(cx).await;
            mouse(cx, Pointer::Up, button, to, 1, modifiers);
        }
        Step::Press(at, modifiers) => {
            mouse(cx, Pointer::Move, MouseButton::Left, at, 0, modifiers);
            frame(cx).await;
            mouse(cx, Pointer::Down, MouseButton::Left, at, 1, modifiers);
        }
        Step::Hold(at, modifiers) => {
            mouse(cx, Pointer::Drag, MouseButton::Left, at, 1, modifiers);
            frame(cx).await;
        }
        Step::Release(at, modifiers) => {
            mouse(cx, Pointer::Up, MouseButton::Left, at, 1, modifiers);
        }
        Step::Scroll {
            at: position,
            delta,
        } => {
            mouse(
                cx,
                Pointer::Move,
                MouseButton::Left,
                position,
                0,
                Modifiers::default(),
            );
            frame(cx).await;
            if let Err(err) = target(cx).and_then(|target| target.send_scroll(position, delta)) {
                eprintln!("jiji: scroll step failed: {err}");
            }
        }
        Step::Key(key) => {
            update(cx, &mut |window, cx| match Keystroke::parse_with_os(
                &key,
                gpui::OperatingSystem::Mac,
            ) {
                Ok(keystroke) => {
                    window.dispatch_keystroke(keystroke, cx);
                }
                Err(err) => eprintln!("jiji: bad key `{key}`: {err}"),
            });
        }
        Step::Type(text) => {
            for ch in text.chars() {
                let key = ch.to_string();
                update(cx, &mut |window, cx| {
                    window.dispatch_keystroke(
                        Keystroke {
                            modifiers: Modifiers::default(),
                            key: key.clone(),
                            key_char: Some(key.clone()),
                        },
                        cx,
                    );
                });
            }
        }
        Step::Wait(ms) => {
            cx.background_executor()
                .timer(Duration::from_millis(ms))
                .await;
        }
        Step::Shot(path) => {
            // Let whatever the previous step changed reach the screen.
            cx.background_executor()
                .timer(Duration::from_millis(180))
                .await;
            update(
                cx,
                &mut |window, _| match crate::capture::window_png(window, &path) {
                    Ok(()) => println!("wrote {}", path.display()),
                    Err(err) => eprintln!("jiji: shot failed: {err}"),
                },
            );
        }
        Step::Resize(w, h) => {
            update(cx, &mut |window, _| window.resize(size(px(w), px(h))));
        }
        Step::Quit => {
            let _ = cx.update(|cx| cx.quit());
        }
    }
}

/// Run the steps against `window`, after the first frames have painted.
pub fn run(steps: Vec<Step>, window: AnyWindowHandle, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(900))
            .await;
        let mut pointer = None;
        for step in steps {
            let resized = matches!(step, Step::Resize(..));
            pointer = step.pointer_after().or(pointer);
            run_step(step, window, cx).await;
            // GPUI re-reads the real pointer whenever a window's bounds
            // change, which would leave hover wherever the user's own
            // pointer happens to rest. Put the scripted one back.
            if let (true, Some((x, y))) = (resized, pointer) {
                frame(cx).await;
                frame(cx).await;
                run_step(Step::Move(x, y), window, cx).await;
            }
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_parse_with_modifiers_and_text_values() {
        let steps = parse("click=10,20; shift-click=1,2;type=a, b;key=cmd-k;wait=50;quit").unwrap();
        assert_eq!(steps.len(), 6);
        assert!(matches!(
            steps[1],
            Step::Click { modifiers, count: 1, .. } if modifiers.shift
        ));
        assert_eq!(steps[2], Step::Type("a, b".into()));
        assert_eq!(steps[3], Step::Key("cmd-k".into()));
        assert_eq!(steps[4], Step::Wait(50));
        assert_eq!(steps[5], Step::Quit);
        assert!(parse("click=1").is_err());
        assert!(parse("teleport=1,2").is_err());
    }
}

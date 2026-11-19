//! Scripted runs: drive the app's window without the pointer or keyboard.
//!
//! Steps go in through GPUI's own event dispatch — the call the platform
//! makes once it has translated an OS event — so they take the path real
//! input does from there on: hit-testing, focus, key bindings, the input
//! handler. Nothing is synthesized at the OS level, and the clipboard is
//! only touched by a step that copies. With `JIJI_HEADLESS=1` the window
//! is never shown at all (see `headless.rs`).
//!
//! `JIJI_SCRIPT` holds the steps, separated by `;`, coordinates in window
//! points:
//!
//!   click=X,Y   dclick=X,Y   tclick=X,Y   rclick=X,Y   mclick=X,Y
//!   shift-click=X,Y   alt-click=X,Y   cmd-click=X,Y
//!   move=X,Y    drag=X0,Y0,X1,Y1   alt-drag=…
//!   press=X,Y   hold=X,Y   release=X,Y   (a drag in parts; alt-hold=…)
//!   scroll=X,Y,DX,DY               key=cmd-k   type=some text
//!   wait=MS     shot=/abs/path.png a11y=/abs/path.json   resize=W,H   quit
//!
//! A `key` is pressed and released, as a keyboard does: Space and Return
//! press a focused control on the release. `a11y` writes the tree a screen
//! reader would read (see `headless::accessibility`).

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    point, px, size, AnyWindowHandle, App, AppContext as _, AsyncApp, InputEvent, KeyUpEvent,
    Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollEvent, ScrollSource, TouchPhase, Window,
};

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
    A11y(PathBuf),
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
            | Step::A11y(_)
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
            "a11y" => {
                steps.push(Step::A11y(PathBuf::from(value)));
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

fn at((x, y): (f32, f32)) -> Point<Pixels> {
    point(px(x), px(y))
}

/// Hand the window one input event, as the platform does with its own.
fn send(event: impl InputEvent, window: AnyWindowHandle, cx: &mut AsyncApp) {
    let delivered = cx.update_window(window, |_, window, cx| {
        window.dispatch_event(event.to_platform_input(), cx);
    });
    if let Err(err) = delivered {
        eprintln!("jiji: step not delivered: {err}");
    }
}

/// The pointer arriving at `to`, `held` if a button is down on the way.
fn moved(to: (f32, f32), held: Option<MouseButton>, modifiers: Modifiers) -> MouseMoveEvent {
    MouseMoveEvent {
        position: at(to),
        pressed_button: held,
        modifiers,
    }
}

fn down(
    button: MouseButton,
    on: (f32, f32),
    clicks: usize,
    modifiers: Modifiers,
) -> MouseDownEvent {
    MouseDownEvent {
        button,
        position: at(on),
        modifiers,
        click_count: clicks,
        first_mouse: false,
    }
}

fn up(button: MouseButton, on: (f32, f32), clicks: usize, modifiers: Modifiers) -> MouseUpEvent {
    MouseUpEvent {
        button,
        position: at(on),
        modifiers,
        click_count: clicks,
    }
}

async fn run_step(step: Step, window: AnyWindowHandle, cx: &mut AsyncApp) {
    let update = |cx: &mut AsyncApp, f: &mut dyn FnMut(&mut Window, &mut App)| {
        let _ = cx.update_window(window, |_, window, cx| f(window, cx));
    };
    let left = MouseButton::Left;
    match step {
        Step::Click {
            at: position,
            count,
            button,
            modifiers,
        } => {
            send(moved(position, None, modifiers), window, cx);
            frame(cx).await;
            // A double click is two presses, the second counted twice —
            // what the platform sends.
            for n in 1..=count {
                send(down(button, position, n, modifiers), window, cx);
                frame(cx).await;
                send(up(button, position, n, modifiers), window, cx);
                frame(cx).await;
            }
        }
        Step::Move(x, y) => send(moved((x, y), None, Modifiers::default()), window, cx),
        Step::Drag {
            from,
            to,
            modifiers,
        } => {
            send(moved(from, None, modifiers), window, cx);
            frame(cx).await;
            send(down(left, from, 1, modifiers), window, cx);
            frame(cx).await;
            // Travel in a few hops: the first leaves the drag threshold,
            // the rest let drop targets react on the way.
            for step in 1..=4 {
                let t = step as f32 / 4.;
                let hop = (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
                send(moved(hop, Some(left), modifiers), window, cx);
                frame(cx).await;
            }
            frame(cx).await;
            send(up(left, to, 1, modifiers), window, cx);
        }
        Step::Press(on, modifiers) => {
            send(moved(on, None, modifiers), window, cx);
            frame(cx).await;
            send(down(left, on, 1, modifiers), window, cx);
        }
        Step::Hold(on, modifiers) => {
            send(moved(on, Some(left), modifiers), window, cx);
            frame(cx).await;
        }
        Step::Release(on, modifiers) => send(up(left, on, 1, modifiers), window, cx),
        Step::Scroll {
            at: position,
            delta: (dx, dy),
        } => {
            send(moved(position, None, Modifiers::default()), window, cx);
            frame(cx).await;
            // One precise delta that belongs to no gesture: what a
            // smooth-scrolling mouse sends, and a trackpad mid-swipe.
            send(
                ScrollEvent {
                    position: at(position),
                    delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                    source: ScrollSource::Pointer,
                },
                window,
                cx,
            );
        }
        Step::Key(key) => {
            update(cx, &mut |window, cx| match Keystroke::parse_with_os(
                &key,
                gpui::OperatingSystem::Mac,
            ) {
                Ok(keystroke) => {
                    window.dispatch_keystroke(keystroke.clone(), cx);
                    window.dispatch_event(KeyUpEvent { keystroke }.to_platform_input(), cx);
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
            // Let whatever the previous step changed reach a frame.
            cx.background_executor()
                .timer(Duration::from_millis(180))
                .await;
            update(
                cx,
                &mut |window, _| match crate::headless::screenshot(window, &path) {
                    Ok(()) => println!("wrote {}", path.display()),
                    Err(err) => eprintln!("jiji: shot failed: {err}"),
                },
            );
        }
        Step::A11y(path) => {
            // The first ask turns accessibility on; the tree is built by the
            // frames after it.
            update(cx, &mut |window, _| {
                crate::headless::accessibility(window);
            });
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            update(
                cx,
                &mut |window, _| match crate::headless::accessibility(window) {
                    Some(tree) => match std::fs::write(&path, tree) {
                        Ok(()) => println!("wrote {}", path.display()),
                        Err(err) => eprintln!("jiji: a11y dump failed: {err}"),
                    },
                    None => eprintln!(
                        "jiji: no accessibility tree to write: {}",
                        crate::headless::HOW
                    ),
                },
            );
        }
        Step::Resize(w, h) => {
            update(cx, &mut |window, _| window.resize(size(px(w), px(h))));
        }
        Step::Quit => cx.update(|cx| cx.quit()),
    }
}

/// Run the steps against `window`, after the first frames have painted.
/// With `quit_after`, the app quits when the steps run out: a window
/// nobody can see has no one to close it.
pub fn run(steps: Vec<Step>, window: AnyWindowHandle, quit_after: bool, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(900))
            .await;
        // GPUI reads the real pointer when it opens a window, and a
        // scripted one never arrives to correct it: start with the
        // pointer off the window, so nothing begins hovered by wherever
        // the user's own pointer rests.
        send(moved((-1., -1.), None, Modifiers::default()), window, cx);
        let mut pointer = None;
        for step in steps {
            let resized = matches!(step, Step::Resize(..));
            pointer = step.pointer_after().or(pointer);
            run_step(step, window, cx).await;
            // It reads it again whenever the window's bounds change. Put
            // the scripted pointer back.
            if resized {
                frame(cx).await;
                frame(cx).await;
                let (x, y) = pointer.unwrap_or((-1., -1.));
                run_step(Step::Move(x, y), window, cx).await;
            }
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
        }
        if quit_after {
            cx.update(|cx| cx.quit());
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
        assert_eq!(
            parse("a11y=/tmp/tree.json").unwrap(),
            vec![Step::A11y("/tmp/tree.json".into())]
        );
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

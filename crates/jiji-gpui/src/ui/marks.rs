//! The graph's node marks as standalone elements, for the places outside
//! the graph that point at a change: the header, a stack's landing order,
//! the operations log. Same shapes, same hues, so the eye carries a change
//! from the map to wherever else it appears.

use gpui::{canvas, fill, point, prelude::*, px, quad, BorderStyle, Bounds, Hsla, PathBuilder};
use jiji_core::snapshot::{NodeKind, RepoSnapshot};

use crate::logic::graph::lane_of;
use crate::logic::inspect::{find_node, stack_position};
use crate::theme::Theme;

/// What a mark says about its change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mark {
    /// The working copy: a ring around a dot — you are here.
    WorkingCopy,
    /// A change on a line: a ring, filled in when it is the one chosen.
    Station { filled: bool },
    /// Immutable history: a diamond.
    Base,
    /// A conflicted change: a disc with the cross cut from it.
    Conflict,
}

/// A mark `size` points across, in `color` (a conflict paints in it too:
/// pass the danger hue).
pub fn node_mark(kind: Mark, color: Hsla, size: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds: Bounds<gpui::Pixels>, (), window, _| {
            let c = bounds.center();
            let r = size / 2.;
            let disc = |radius: f32| {
                Bounds::from_corners(
                    point(c.x - px(radius), c.y - px(radius)),
                    point(c.x + px(radius), c.y + px(radius)),
                )
            };
            match kind {
                Mark::WorkingCopy => {
                    let ring = (size * 0.15).max(1.5);
                    window.paint_quad(quad(
                        disc(r),
                        px(r),
                        gpui::transparent_black(),
                        px(ring),
                        color,
                        BorderStyle::Solid,
                    ));
                    let inner = r * 0.46;
                    window.paint_quad(fill(disc(inner), color).corner_radii(px(inner)));
                }
                Mark::Station { filled } => {
                    let ring = (size * 0.2).max(1.5);
                    window.paint_quad(quad(
                        disc(r),
                        px(r),
                        if filled {
                            color
                        } else {
                            gpui::transparent_black()
                        },
                        px(ring),
                        color,
                        BorderStyle::Solid,
                    ));
                }
                Mark::Base => {
                    let d = px(r * 0.95);
                    let mut builder = PathBuilder::fill();
                    builder.move_to(point(c.x, c.y - d));
                    builder.line_to(point(c.x + d, c.y));
                    builder.line_to(point(c.x, c.y + d));
                    builder.line_to(point(c.x - d, c.y));
                    builder.close();
                    if let Ok(path) = builder.build() {
                        window.paint_path(path, color);
                    }
                }
                Mark::Conflict => {
                    window.paint_quad(fill(disc(r), color).corner_radii(px(r)));
                    let xr = px(r * 0.42);
                    for (from, to) in [
                        (point(c.x - xr, c.y - xr), point(c.x + xr, c.y + xr)),
                        (point(c.x + xr, c.y - xr), point(c.x - xr, c.y + xr)),
                    ] {
                        let mut builder = PathBuilder::stroke(px((size * 0.13).max(1.2)));
                        builder.move_to(from);
                        builder.line_to(to);
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, gpui::white());
                        }
                    }
                }
            }
        },
    )
    .size(px(size))
    .flex_shrink_0()
}

/// The hue of the line a change rides, when it rides one.
pub fn line_of(snapshot: &RepoSnapshot, id: &str, t: &Theme) -> Option<Hsla> {
    let position = stack_position(snapshot, id)?;
    Some(t.lane(lane_of(snapshot, &position.workstream.id).unwrap_or(0)))
}

/// How the graph marks the change `id`, and in what hue; `None` when the
/// snapshot does not draw it.
pub fn mark_for(snapshot: &RepoSnapshot, id: &str, t: &Theme) -> Option<(Mark, Hsla)> {
    let node = find_node(snapshot, id)?;
    Some(if node.kind == NodeKind::WorkingCopy {
        (Mark::WorkingCopy, t.working_copy)
    } else if node.has_conflict {
        (Mark::Conflict, t.danger)
    } else if node.kind == NodeKind::Immutable {
        (Mark::Base, Theme::blend(t.text3, 0.8, t.bg1))
    } else {
        (
            Mark::Station { filled: false },
            line_of(snapshot, id, t).unwrap_or(t.accent),
        )
    })
}

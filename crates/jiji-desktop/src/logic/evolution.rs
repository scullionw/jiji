//! The evolution timeline's logic: the selected span of rewrite steps, and
//! how clicks and arrow keys move it. A span names two versions by index
//! (oldest first); the surface shows the diff `versions[from] →
//! versions[to]`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub from: usize,
    pub to: usize,
}

/// Where the pane opens: the newest rewrite step — "what did I just
/// change". `None` when the change has a single version (no step exists).
pub fn default_span(count: usize) -> Option<Span> {
    (count >= 2).then(|| Span {
        from: count - 2,
        to: count - 1,
    })
}

/// The single step that produced version `i` (clicking its dot). The
/// oldest version has no producing step, so it selects the first step —
/// the click still lands somewhere sensible.
pub fn step_to(i: usize, count: usize) -> Option<Span> {
    if count < 2 {
        return None;
    }
    let to = i.clamp(1, count - 1);
    Some(Span { from: to - 1, to })
}

/// Widen `span` to include version `i` (⇧-click). Landing inside the span
/// narrows toward the clicked version from the far end, so ⇧-click always
/// answers "measure to here".
pub fn extend_to(span: Span, i: usize, count: usize) -> Span {
    let at = i.min(count.saturating_sub(1));
    if at < span.from {
        Span {
            from: at,
            to: span.to,
        }
    } else if at > span.to {
        Span {
            from: span.from,
            to: at,
        }
    } else if at - span.from >= span.to - at {
        // Inside: keep the anchor further from the click.
        Span {
            from: span.from,
            to: at.max(span.from + 1),
        }
    } else {
        Span {
            from: at.min(span.to - 1),
            to: span.to,
        }
    }
}

/// Scrub by one step (←/→): the selection collapses to the single step
/// ending at the moved edge, sliding along the chain.
pub fn scrub(span: Span, delta: isize, count: usize) -> Option<Span> {
    if count < 2 {
        return None;
    }
    let single = span.to - span.from == 1;
    let moved = if single {
        span.to as isize + delta
    } else {
        span.to as isize
    };
    let to = moved.clamp(1, count as isize - 1) as usize;
    Some(Span { from: to - 1, to })
}

/// Grow one edge by a step (⇧←/⇧→); already at the end, nothing moves.
pub fn grow(span: Span, older: bool, count: usize) -> Span {
    if older {
        Span {
            from: span.from.saturating_sub(1),
            to: span.to,
        }
    } else {
        Span {
            from: span.from,
            to: (span.to + 1).min(count.saturating_sub(1)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(from: usize, to: usize) -> Span {
        Span { from, to }
    }

    #[test]
    fn the_pane_opens_on_the_newest_step() {
        assert_eq!(default_span(1), None);
        assert_eq!(default_span(2), Some(span(0, 1)));
        assert_eq!(default_span(5), Some(span(3, 4)));
    }

    #[test]
    fn a_dot_selects_the_step_that_produced_it() {
        assert_eq!(step_to(3, 5), Some(span(2, 3)));
        // The oldest version has no producing step.
        assert_eq!(step_to(0, 5), Some(span(0, 1)));
        assert_eq!(step_to(9, 5), Some(span(3, 4)));
        assert_eq!(step_to(0, 1), None);
    }

    #[test]
    fn shift_click_measures_to_the_clicked_version() {
        assert_eq!(extend_to(span(2, 3), 0, 6), span(0, 3));
        assert_eq!(extend_to(span(2, 3), 5, 6), span(2, 5));
        // Inside the span: the far anchor stays.
        assert_eq!(extend_to(span(0, 5), 4, 6), span(0, 4));
        assert_eq!(extend_to(span(0, 5), 1, 6), span(1, 5));
        // Never collapses to nothing.
        assert_eq!(extend_to(span(2, 3), 2, 6), span(2, 3));
    }

    #[test]
    fn arrows_scrub_single_steps_and_collapse_wide_spans() {
        assert_eq!(scrub(span(2, 3), -1, 6), Some(span(1, 2)));
        assert_eq!(scrub(span(2, 3), 1, 6), Some(span(3, 4)));
        // At the ends, nothing moves.
        assert_eq!(scrub(span(0, 1), -1, 6), Some(span(0, 1)));
        assert_eq!(scrub(span(4, 5), 1, 6), Some(span(4, 5)));
        // A wide span collapses onto its newest step first.
        assert_eq!(scrub(span(1, 4), -1, 6), Some(span(3, 4)));
    }

    #[test]
    fn shift_arrows_grow_one_edge() {
        assert_eq!(grow(span(2, 3), true, 6), span(1, 3));
        assert_eq!(grow(span(2, 3), false, 6), span(2, 4));
        assert_eq!(grow(span(0, 5), true, 6), span(0, 5));
        assert_eq!(grow(span(0, 5), false, 6), span(0, 5));
    }
}

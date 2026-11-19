//! Spell checking for a text field, through the system's checker: the
//! words it flags, kept in step with the text as it is typed, and the
//! dotted red line Mac text wears under a misspelling.

use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui::{fill, point, px, size, Bounds, Hsla, Pixels, Task, Window};

/// How long typing must pause before the text is checked again.
pub(super) const PAUSE: Duration = Duration::from_millis(250);

/// What a field asks of a spell checker.
pub trait Speller {
    /// The misspelled words of `text`, as byte ranges.
    fn misspelled(&self, text: &str) -> Vec<Range<usize>>;
    /// What the word at `word` might have been meant to be, best first.
    fn guesses(&self, text: &str, word: Range<usize>) -> Vec<String>;
    /// Stop flagging `word` in this field.
    fn ignore(&self, word: &str);
    /// Add `word` to the user's dictionary.
    fn learn(&self, word: &str);
}

impl Speller for crate::appkit::SpellDocument {
    fn misspelled(&self, text: &str) -> Vec<Range<usize>> {
        crate::appkit::SpellDocument::misspelled(self, text)
    }

    fn guesses(&self, text: &str, word: Range<usize>) -> Vec<String> {
        crate::appkit::SpellDocument::guesses(self, text, word)
    }

    fn ignore(&self, word: &str) {
        crate::appkit::SpellDocument::ignore(self, word)
    }

    fn learn(&self, word: &str) {
        crate::appkit::SpellDocument::learn(self, word)
    }
}

/// The system's checker, for one field: `None` when the user has turned
/// spell checking while typing off. A test gets one that knows a few
/// misspellings and never reaches the system's.
pub fn system() -> Option<Rc<dyn Speller>> {
    #[cfg(not(test))]
    {
        crate::appkit::SpellDocument::open().map(|document| Rc::new(document) as Rc<dyn Speller>)
    }
    #[cfg(test)]
    {
        Some(Rc::new(tests::Known::default()))
    }
}

/// A field's spelling: its checker and what it flagged.
pub(super) struct Spelling {
    pub speller: Rc<dyn Speller>,
    /// The words flagged at the last check, carried along by the edits
    /// since.
    pub flagged: Vec<Range<usize>>,
    /// Where the last edit left off typing: the word that ends there is
    /// still being written, and is not judged until it is finished.
    pub typed_to: Option<usize>,
    /// The check waiting for typing to pause.
    pub pending: Option<Task<()>>,
}

impl Spelling {
    pub fn new(speller: Rc<dyn Speller>) -> Self {
        Self {
            speller,
            flagged: Vec::new(),
            typed_to: None,
            pending: None,
        }
    }

    /// `range` was replaced by `inserted`: the flags after it move with the
    /// text, and the ones it changed wait for the next check. A space or a
    /// stop typed at a word's edge leaves that word as it was.
    pub fn edited(&mut self, range: Range<usize>, inserted: &str) {
        let joins = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || "'’_".contains(c));
        let apart_after = range.is_empty() && !joins(inserted.chars().next());
        let apart_before = range.is_empty() && !joins(inserted.chars().next_back());
        let delta = inserted.len() as isize - range.len() as isize;
        self.flagged.retain_mut(|word| {
            if word.end < range.start || (word.end == range.start && apart_after) {
                true
            } else if word.start > range.end || (word.start == range.end && apart_before) {
                word.start = word.start.saturating_add_signed(delta);
                word.end = word.end.saturating_add_signed(delta);
                true
            } else {
                false
            }
        });
        self.typed_to = (!inserted.is_empty()).then_some(range.start + inserted.len());
    }

    /// The flags to draw: not the word still being typed.
    pub fn shown(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.flagged
            .iter()
            .filter(|word| Some(word.end) != self.typed_to)
            .cloned()
    }

    /// The flagged word at `offset`, a caret's position: inside it or at
    /// either end.
    pub fn word_at(&self, offset: usize) -> Option<Range<usize>> {
        self.flagged
            .iter()
            .find(|word| word.start <= offset && offset <= word.end)
            .cloned()
    }
}

/// The dotted line under a misspelled run: round dots along the box's
/// foot, just under the baseline at `baseline` from its top.
pub(super) fn paint_dots(run: Bounds<Pixels>, baseline: Pixels, color: Hsla, window: &mut Window) {
    const DOT: f32 = 2.5;
    const PITCH: f32 = 4.;
    let y = run.top() + baseline + px(1.5);
    let width: f32 = run.size.width.into();
    let count = ((width + PITCH - DOT) / PITCH).floor().max(1.);
    // Centred under the word, so short words do not lean left.
    let mut x = run.left() + px((width - ((count - 1.) * PITCH + DOT)) / 2.);
    for _ in 0..count as usize {
        window.paint_quad(
            fill(Bounds::new(point(x, y), size(px(DOT), px(DOT))), color)
                .corner_radii(px(DOT / 2.)),
        );
        x += px(PITCH);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::ops::Range;

    use super::{Speller, Spelling};

    /// The tests' checker: it knows "teh" and "wrod" are wrong, and
    /// remembers what it was told to ignore or learn.
    #[derive(Default)]
    pub struct Known {
        pub accepted: RefCell<HashSet<String>>,
    }

    const WRONG: [(&str, &[&str]); 2] = [("teh", &["the", "ten", "tea"]), ("wrod", &["word"])];

    impl Speller for Known {
        fn misspelled(&self, text: &str) -> Vec<Range<usize>> {
            let accepted = self.accepted.borrow();
            unicode_segmentation::UnicodeSegmentation::split_word_bound_indices(text)
                .filter(|(_, word)| {
                    WRONG.iter().any(|(wrong, _)| wrong == word) && !accepted.contains(*word)
                })
                .map(|(start, word)| start..start + word.len())
                .collect()
        }

        fn guesses(&self, text: &str, word: Range<usize>) -> Vec<String> {
            WRONG
                .iter()
                .find(|(wrong, _)| *wrong == &text[word.clone()])
                .map(|(_, guesses)| guesses.iter().map(|guess| guess.to_string()).collect())
                .unwrap_or_default()
        }

        fn ignore(&self, word: &str) {
            self.accepted.borrow_mut().insert(word.to_owned());
        }

        fn learn(&self, word: &str) {
            self.accepted.borrow_mut().insert(word.to_owned());
        }
    }

    #[test]
    fn flags_move_with_the_text_and_wait_where_it_changed() {
        let mut spelling = Spelling::new(std::rc::Rc::new(Known::default()));
        // "fix teh wrod now": two flags.
        spelling.flagged = vec![4..7, 8..12];
        // Two letters typed at the start move both.
        spelling.edited(0..0, "a ");
        assert_eq!(spelling.flagged, [6..9, 10..14]);
        // An edit inside the first drops it until the next check.
        spelling.edited(7..8, "x");
        assert_eq!(spelling.flagged, [10..14]);
        // A letter typed onto a word's end changes the word; a space does
        // not, and the space typed before one moves it.
        spelling.flagged = vec![0..3, 10..14];
        spelling.edited(14..14, "s");
        assert_eq!(spelling.flagged, [0..3]);
        spelling.flagged = vec![0..3, 10..14];
        spelling.edited(14..14, " ");
        assert_eq!(spelling.flagged, [0..3, 10..14]);
        spelling.edited(10..10, " ");
        assert_eq!(spelling.flagged, [0..3, 11..15]);
        // The word that ends where typing stopped is not drawn yet.
        spelling.typed_to = Some(15);
        assert_eq!(spelling.shown().collect::<Vec<_>>(), [0..3]);
        assert_eq!(spelling.word_at(15), Some(11..15));
        assert_eq!(spelling.word_at(5), None);
    }
}

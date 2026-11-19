//! The split tray: carve a change in two by file, hunk or line, or move
//! the picked part into another change.

use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, Hsla, Role, Toggled, Window};
use jiji_core::snapshot::{FileDiff, GraphNode, RepoSnapshot};

use super::title_or_none;
use crate::logic::inspect::{
    bookmarks_at, descendants_of, find_node, move_direction, split_path, MoveDirection,
};
use crate::logic::split::{
    self, can_select_hunks, can_select_lines, changed_line_indices, file_hunks, hunk_label,
    hunk_preview, hunk_stats, line_text, movable, split_summary, splittable, HunkPick, SplitPick,
};
use crate::logic::text::{counted, short_id};
use crate::theme::{theme, RADIUS_M, RADIUS_S, TEXT_S, TEXT_XS};
use crate::ui::focus::FocusRow;
use crate::ui::input::grip;
use crate::ui::ScrollY as _;
use crate::ui::{
    btn, h_flex, icon, scrollbar, segmented, tooltip, truncated, v_flex, IconName, Segment,
    Tabbable as _,
};
use crate::views::workbench::WorkbenchView;

impl WorkbenchView {
    pub(super) fn render_split_tray(
        &mut self,
        snapshot: &RepoSnapshot,
        node: &GraphNode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let acting = self.header.acting;
        let into = self.header.split_into;
        let title = self.tray_title(
            &format!("Split **{}**", short_id(&node.id, 4)),
            format!("\u{201c}{}\u{201d}", title_or_none(&node.description)),
            cx,
        );
        let files: Option<Vec<FileDiff>> = self.header_files().map(<[FileDiff]>::to_vec);
        let quiet = |text: &'static str| {
            div()
                .mt(px(8.))
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(text)
        };
        let Some(files) = files else {
            let cancel = self.cancel_button("Cancel", cx);
            return self
                .tray(false, "Split this change", cx)
                .child(title)
                .child(quiet("Loading the file list…"))
                .child(self.confirm_row(String::new(), vec![cancel], cx))
                .into_any_element();
        };
        if !movable(&files) {
            let cancel = self.cancel_button("Cancel", cx);
            return self
                .tray(false, "Split this change", cx)
                .child(title)
                .child(quiet(
                    "This change touches no files — there is nothing to split or move.",
                ))
                .child(self.confirm_row(String::new(), vec![cancel], cx))
                .into_any_element();
        }
        let summary = split_summary(Some(&files), &self.header.split);
        let ready = self.split_ready();
        let marks = bookmarks_at(snapshot, &node.id);
        let names = marks
            .iter()
            .map(|m| m.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let descendants = descendants_of(snapshot, &node.id);
        let wc_above = descendants.iter().any(|d| d.id == snapshot.working_copy);
        let node_title = title_or_none(&node.description).to_owned();

        // "the 2 checked files", "checked 1 hunk across 1 file", …
        let kept = {
            let n = counted;
            if summary.partial == 0 {
                n(summary.whole, "checked file")
            } else {
                let mut sub = Vec::new();
                if summary.hunks > 0 {
                    sub.push(n(summary.hunks, "hunk"));
                }
                if summary.lines > 0 {
                    sub.push(n(summary.lines, "line"));
                }
                let sub = sub.join(" and ");
                if summary.whole == 0 {
                    format!("checked {sub} across {}", n(summary.partial, "file"))
                } else {
                    format!(
                        "{} plus {sub} from {} more",
                        n(summary.whole, "checked file"),
                        summary.partial
                    )
                }
            }
        };
        let kept_singular = if summary.partial == 0 {
            summary.whole == 1
        } else {
            summary.whole == 0 && summary.hunks + summary.lines == 1
        };
        let unchecked_note = if summary.partial > 0 {
            " — including the other hunks of partially checked files —"
        } else {
            ""
        };

        let this = cx.entity().downgrade();
        let mut controls = h_flex().mt(px(8.)).gap(px(8.)).flex_wrap().child(segmented(
            "split-mode",
            "Where the checked selection goes",
            vec![
                Segment::text(false, "Into a new change"),
                Segment::text(true, "Into an existing change"),
            ],
            &into,
            cx,
            move |into, window, cx| {
                let into = *into;
                let _ = this.update(cx, |this, cx| {
                    this.header.split_into = into;
                    this.header.error = None;
                    this.header.dest = None;
                    if into {
                        this.focus_filter_or_tray(window, cx);
                    }
                    cx.notify();
                });
            },
        ));
        if into {
            controls = controls.children(self.dest_filter(window, cx));
        }
        let mut tray = self
            .tray(false, "Split this change", cx)
            .child(title)
            .child(controls);

        if !into && !splittable(&files) {
            tray = tray.child(quiet(
                "This change touches only one file, and its diff offers nothing to carve apart — nothing to split. “Into an existing change” can still move it somewhere else.",
            ));
        } else {
            tray = tray
                .child(self.label(
                    if into {
                        "Check the files — or single hunks — to move into the destination"
                    } else {
                        "Check the files — or single hunks — to carve off into their own change"
                    },
                    cx,
                ))
                .child(self.split_file_list(&files, window, cx));
            if !into {
                let focused = self.header.split_desc.read(cx).is_focused(window);
                tray = tray
                    .child(self.label("Description for the split-off change", cx))
                    .child(
                        div()
                            .relative()
                            .flex()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(RADIUS_M))
                            .border_1()
                            .border_color(if focused { t.accent } else { t.border1 })
                            .bg(t.bg1)
                            .font_family(t.mono.clone())
                            .text_size(px(TEXT_S))
                            .line_height(px(18.))
                            .debug_selector(|| "split-description-field".into())
                            .child(self.header.split_desc.clone())
                            .child(grip(
                                &self.header.split_desc,
                                "Resize description",
                                window,
                                cx,
                            )),
                    );
                if summary.whole + summary.partial > 0 {
                    let mut items = vec![(
                        format!(
                            "The {kept} become{} the first change — it keeps the id **{}** and the description above.",
                            if kept_singular { "s" } else { "" },
                            short_id(&node.id, 4)
                        ),
                        false,
                    )];
                    if summary.all_covered {
                        items.push((
                            "Every file is fully checked — leave a file or a hunk for the new change on top."
                                .into(),
                            true,
                        ));
                    } else {
                        items.push((
                            format!(
                                "Everything unchecked{unchecked_note} moves to a new change directly on top, keeping ~~\u{201c}{node_title}\u{201d}~~."
                            ),
                            false,
                        ));
                    }
                    if !marks.is_empty() {
                        items.push((
                            format!(
                                "Bookmark{} {names} follow{} the new top change.",
                                if marks.len() == 1 { "" } else { "s" },
                                if marks.len() == 1 { "s" } else { "" }
                            ),
                            false,
                        ));
                    }
                    if node.id == snapshot.working_copy {
                        items.push((
                            "The working copy follows the new top change — nothing moves on disk."
                                .into(),
                            false,
                        ));
                    } else if wc_above {
                        items.push(("The working copy follows the rebase.".into(), false));
                    }
                    if !descendants.is_empty() {
                        items.push((
                            format!(
                                "{} descendant change{} onto the new top change.",
                                descendants.len(),
                                if descendants.len() == 1 {
                                    " rebases"
                                } else {
                                    "s rebase"
                                }
                            ),
                            false,
                        ));
                    }
                    tray = tray.child(self.consequences(items, cx));
                }
            } else {
                let list =
                    self.dest_list("Move destination", "No matching destination", window, cx);
                tray = tray
                    .child(self.label("Destination — the change the selection moves into", cx))
                    .child(list);
                let dest = self
                    .header
                    .dest
                    .as_deref()
                    .and_then(|id| find_node(snapshot, id));
                if let (Some(dest), true) = (dest, summary.whole + summary.partial > 0) {
                    let direction = move_direction(snapshot, &node.id, &dest.id);
                    let where_ = match direction {
                        Some(MoveDirection::Backwards) => " — an earlier change beneath this one",
                        Some(MoveDirection::Forward) => " — a later change building on this one",
                        Some(MoveDirection::Sideways) => " — a change on another branch",
                        None => "",
                    };
                    let mut items = vec![(
                        format!(
                            "The {kept} move{} into **{}** ~~\u{201c}{}\u{201d}~~{where_}.",
                            if kept_singular { "s" } else { "" },
                            short_id(&dest.id, 4),
                            title_or_none(&dest.description)
                        ),
                        false,
                    )];
                    if summary.all_covered {
                        let bookmarks = if marks.is_empty() {
                            String::new()
                        } else {
                            format!(
                                ", and bookmark{} {names} move{} to its parent",
                                if marks.len() == 1 { "" } else { "s" },
                                if marks.len() == 1 { "s" } else { "" }
                            )
                        };
                        items.push((
                            format!(
                                "That is everything in **{}** — the emptied change is abandoned{bookmarks}.",
                                short_id(&node.id, 4)
                            ),
                            false,
                        ));
                        if node.id == snapshot.working_copy {
                            items.push((
                                "The working copy respawns as a new empty change on its parent."
                                    .into(),
                                false,
                            ));
                        }
                    } else {
                        items.push((
                            format!(
                                "Everything unchecked{unchecked_note} stays here as ~~\u{201c}{node_title}\u{201d}~~."
                            ),
                            false,
                        ));
                        if node.id == snapshot.working_copy {
                            items.push((
                                if direction == Some(MoveDirection::Backwards) {
                                    "The working copy keeps building on the moved changes — nothing moves on disk."
                                } else {
                                    "The moved changes leave the working copy's files on disk."
                                }
                                .into(),
                                false,
                            ));
                        }
                    }
                    if dest.id == snapshot.working_copy {
                        items.push((
                            "The working copy takes the changes; its files update on disk.".into(),
                            false,
                        ));
                    } else if node.id != snapshot.working_copy && wc_above {
                        items.push(("The working copy follows the rebase.".into(), false));
                    }
                    items.push((
                        "Changes that no longer apply cleanly become conflicts instead of blocking the move; the operation log can undo it."
                            .into(),
                        false,
                    ));
                    tray = tray.child(self.consequences(items, cx));
                }
            }
        }
        let hint = if into {
            if ready {
                "↵ to confirm"
            } else if summary.whole + summary.partial == 0 {
                "Check the files or hunks to move"
            } else {
                "Pick the change they move into — ↑↓ or click"
            }
        } else if ready {
            "⌘↵ to confirm"
        } else {
            "Check files or hunks for the first change — leaving something on each side"
        };
        let buttons = vec![
            self.cancel_button("Cancel", cx),
            btn(
                "confirm-go",
                match (acting, into) {
                    (true, true) => "Moving…",
                    (true, false) => "Splitting…",
                    (false, true) => "Move here",
                    (false, false) => "Split",
                },
            )
            .primary()
            .disabled(acting || !ready)
            .on_click(cx.listener(|this, _, window, cx| this.submit_split(window, cx)))
            .into_any_element(),
        ];
        tray.child(self.confirm_row(hint.into(), buttons, cx))
            .into_any_element()
    }

    /// The split panel's checklist: files, their hunks on demand, a
    /// hunk's changed lines on demand.
    pub(super) fn split_file_list(
        &mut self,
        files: &[FileDiff],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let acting = self.header.acting;
        let check = |state: Option<bool>| {
            // Checked, partly checked, or empty.
            let mut cell = div()
                .w(px(14.))
                .h(px(14.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .border_1();
            cell = match state {
                Some(false) => cell.border_color(t.border1).bg(t.bg_well),
                Some(true) => cell.border_color(t.accent).bg(t.accent).child(icon(
                    IconName::Check,
                    10.,
                    t.accent_contrast,
                )),
                None => cell.border_color(t.accent).bg(t.accent).child(icon(
                    IconName::Minus,
                    10.,
                    t.accent_contrast,
                )),
            };
            cell
        };
        // A row checks or unchecks as a whole: to assistive technology and
        // the keyboard it is the checkbox, named for what it carries.
        let row =
            |id: gpui::ElementId, indent: f32, on: bool, state: Option<bool>, name: String| {
                let mut row = h_flex()
                    .id(id)
                    .role(Role::CheckBox)
                    .aria_label(name)
                    .aria_toggled(match state {
                        Some(true) => Toggled::True,
                        Some(false) => Toggled::False,
                        None => Toggled::Mixed,
                    })
                    .flex_1()
                    .min_w_0()
                    .h(px(24.))
                    .gap(px(8.))
                    .pl(px(8. + indent))
                    .pr(px(8.))
                    .rounded(px(RADIUS_S))
                    .text_size(px(TEXT_S));
                if !acting {
                    row = row.tabbable_in(t.accent).cursor_pointer();
                }
                if on {
                    row.bg(t.accent_dim)
                } else {
                    row.hover(|style| style.bg(t.bg_hover))
                }
            };
        let expander = |id: gpui::ElementId, label: String, open: bool| {
            h_flex()
                .id(id)
                .role(Role::Button)
                .aria_label(label.clone())
                .aria_expanded(open)
                .tabbable_in(t.accent)
                .flex_shrink_0()
                .gap(px(4.))
                .h(px(20.))
                .px(px(6.))
                .rounded(px(RADIUS_S))
                .font_family(t.mono.clone())
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .cursor_pointer()
                .hover(|style| style.bg(t.bg_hover).text_color(t.text1))
                .child(label)
                .child(icon(
                    if open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    },
                    10.,
                    t.text3,
                ))
        };
        let stat = |sign: &str, n: u32, color: Hsla| {
            (n > 0).then(|| div().text_color(color).child(format!("{sign}{n}")))
        };
        let mut list = v_flex()
            .id("split-checklist")
            .role(Role::Group)
            .aria_label("Files and hunks to split off");
        let mut key = 0usize;
        for file in files {
            let pick = self.header.split.get(&file.path);
            let checked = pick == Some(&SplitPick::All);
            let partial = matches!(pick, Some(SplitPick::Hunks(_)));
            let hunky = can_select_hunks(file);
            let hunks_open = hunky && self.header.split_hunks_open.contains(&file.path);
            let (dir, name) = split_path(&file.path);
            let (glyph, hue) = match file.status {
                jiji_core::snapshot::FileStatus::Added => ("A", t.add_fg),
                jiji_core::snapshot::FileStatus::Modified => ("M", t.text3),
                jiji_core::snapshot::FileStatus::Removed => ("D", t.del_fg),
                _ => ("R", t.accent_strong),
            };
            let path = file.path.clone();
            key += 1;
            let state = if checked {
                Some(true)
            } else if partial {
                None
            } else {
                Some(false)
            };
            let kept = FocusRow::new(("split-file-line", key), window, cx);
            let mut line = h_flex().id(("split-file-line", key)).gap(px(4.)).child(
                row(
                    ("split-file", key).into(),
                    0.,
                    checked,
                    state,
                    file.path.clone(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.header.acting {
                        split::toggle_file(&mut this.header.split, &path);
                        cx.notify();
                    }
                }))
                .child(check(state))
                .child(
                    div()
                        .w(px(12.))
                        .flex_shrink_0()
                        .font_family(t.mono.clone())
                        .text_size(px(TEXT_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(hue)
                        .child(glyph),
                )
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .font_family(t.mono.clone())
                        .child(
                            div()
                                .min_w_0()
                                .text_color(t.text3)
                                .child(truncated(dir.to_owned())),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(t.text1)
                                .child(name.to_owned()),
                        ),
                ),
            );
            if hunky {
                let path = file.path.clone();
                line = line.child(
                    expander(
                        ("split-expand", key).into(),
                        format!("{} hunks", file_hunks(file).len()),
                        hunks_open,
                    )
                    .tooltip(tooltip(if hunks_open {
                        "Hide this file's hunks"
                    } else {
                        "Pick single hunks of this file"
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.header.split_hunks_open.remove(&path) {
                            this.header.split_hunks_open.insert(path.clone());
                        }
                        cx.notify();
                    })),
                );
            }
            list = list.child(kept.row(line, &self.header.split_scroll, window, cx));
            if !hunks_open {
                continue;
            }
            let hunks = file_hunks(file);
            for (index, hunk) in hunks.iter().enumerate() {
                let hunk_pick = match pick {
                    Some(SplitPick::Hunks(map)) => map.get(&index),
                    _ => None,
                };
                let on = checked || hunk_pick == Some(&HunkPick::All);
                let half = matches!(hunk_pick, Some(HunkPick::Lines(_)));
                let (added, removed) = hunk_stats(hunk);
                let line_key = format!("{}@{index}", file.path);
                let liney = can_select_lines(hunk);
                let lines_open = liney && self.header.split_lines_open.contains(&line_key);
                let (path, count) = (file.path.clone(), hunks.len());
                key += 1;
                let state = if on {
                    Some(true)
                } else if half {
                    None
                } else {
                    Some(false)
                };
                let name = format!(
                    "Hunk {} of {}, {}, {added} added, {removed} removed",
                    index + 1,
                    hunks.len(),
                    hunk_label(hunk)
                );
                let kept = FocusRow::new(("split-hunk-line", key), window, cx);
                let mut line = h_flex().id(("split-hunk-line", key)).gap(px(4.)).child(
                    row(("split-hunk", key).into(), 22., on, state, name)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.header.acting {
                                split::toggle_hunk(&mut this.header.split, &path, index, count);
                                cx.notify();
                            }
                        }))
                        .child(check(state))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .text_color(t.text2)
                                .child(hunk_label(hunk)),
                        )
                        .child(
                            h_flex()
                                .flex_shrink_0()
                                .gap(px(6.))
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .children(stat("+", added, t.add_fg))
                                .children(stat("−", removed, t.del_fg)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .text_color(t.text3)
                                .child(truncated(hunk_preview(hunk))),
                        ),
                );
                if liney {
                    let toggle_key = line_key.clone();
                    line = line.child(
                        expander(
                            ("split-expand-lines", key).into(),
                            format!("{} lines", changed_line_indices(hunk).len()),
                            lines_open,
                        )
                        .tooltip(tooltip(if lines_open {
                            "Hide this hunk's lines"
                        } else {
                            "Pick single lines of this hunk"
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.header.split_lines_open.remove(&toggle_key) {
                                this.header.split_lines_open.insert(toggle_key.clone());
                            }
                            cx.notify();
                        })),
                    );
                }
                list = list.child(kept.row(line, &self.header.split_scroll, window, cx));
                if !lines_open {
                    continue;
                }
                for line_index in changed_line_indices(hunk) {
                    let source = &hunk.lines[line_index as usize];
                    let line_on = on
                        || matches!(hunk_pick, Some(HunkPick::Lines(set)) if set.contains(&line_index));
                    let added = source.kind == jiji_core::snapshot::DiffLineKind::Added;
                    let target = file.clone();
                    key += 1;
                    let name = format!(
                        "{} line: {}",
                        if added { "Added" } else { "Removed" },
                        line_text(source)
                    );
                    let kept = FocusRow::new(("split-line-row", key), window, cx);
                    let line = h_flex().id(("split-line-row", key)).child(
                        row(
                            ("split-line", key).into(),
                            44.,
                            line_on,
                            Some(line_on),
                            name,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.header.acting {
                                split::toggle_line(
                                    &mut this.header.split,
                                    &target,
                                    index,
                                    line_index,
                                );
                                cx.notify();
                            }
                        }))
                        .child(check(Some(line_on)))
                        .child(
                            div()
                                .w(px(10.))
                                .flex_shrink_0()
                                .font_family(t.mono.clone())
                                .text_color(if added { t.add_fg } else { t.del_fg })
                                .child(if added { "+" } else { "−" }),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(t.mono.clone())
                                .text_size(px(TEXT_XS))
                                .text_color(t.text2)
                                .child(truncated(line_text(source))),
                        ),
                    );
                    list = list.child(kept.row(line, &self.header.split_scroll, window, cx));
                }
            }
        }
        div()
            .relative()
            .rounded(px(RADIUS_M))
            .border_1()
            .border_color(t.border2)
            .bg(t.bg1)
            .child(
                div()
                    .id("split-files")
                    .max_h(px(220.))
                    .p(px(2.))
                    .scroll_y()
                    .track_scroll(&self.header.split_scroll)
                    .child(list),
            )
            .child(scrollbar(
                "split-files-scrollbar",
                &self.header.split_scroll,
            ))
            .into_any_element()
    }
}

//! A change's life over time (`jj evolog`) as a first-class surface: a
//! scrubbable timeline of every version the change was rewritten through,
//! with the selected step's diff on the real diff surface. Built for the
//! `jj edit` workflow, where "what did I just change" is the newest
//! rewrite step, not the change's whole diff — the pane opens there. ←/→
//! scrub through time; ⇧-click (or ⇧←/⇧→) widens the span to read several
//! rewrites as one diff; Restore returns the change to the version under
//! the cursor.
//!
//! Versions are addressed by commit id — content-stable, and hidden ids
//! still resolve — so fetched step diffs never go stale; new operations
//! only grow the chain. The timeline wears each step's weight (+/−) as
//! the step diffs arrive, so the big rewrites stand out at a glance.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, FontWeight, ScrollHandle, SharedString,
    Window,
};
use jiji_core::snapshot::{ChangeEvolog, EvologEntry, NodeKind};

use super::WorkbenchView;
use crate::app::DiffLayout;
use crate::diff::model::Prepared;
use crate::diff::view::{DiffEvent, DiffSurface};
use crate::host::CommandError;
use crate::logic::evolution::{default_span, extend_to, grow, scrub, step_to, Span};
use crate::logic::inspect::{find_node, summary_of};
use crate::logic::text::short_id;
use crate::theme::{theme, TEXT_S, TEXT_XS};
use crate::ui::{
    absolute_time, btn, empty_state, from_now, h_flex, icon, icon_btn, label_box, short_age,
    tooltip, truncated, v_flex, IconName, NodeExt as _, Tabbable as _,
};

/// Warm every step's diff once the chain is known — capped so a
/// pathological chain stops asking; steps beyond the cap still fetch on
/// demand when selected.
const PREFETCH_CAP: usize = 32;

/// A fetched step or span diff, laid out, with its weight for the
/// timeline.
struct Held {
    prepared: Prepared,
    added: u32,
    removed: u32,
}

enum Entry {
    Loading,
    Ready(Held),
    Failed(String),
}

pub(super) struct EvolutionState {
    /// What the pane was last asked for: change and operation.
    key: Option<(String, String)>,
    evolog: Option<Arc<ChangeEvolog>>,
    /// Versions oldest → newest — the timeline's reading order.
    versions: Vec<EvologEntry>,
    error: Option<String>,
    seq: u64,
    span: Option<Span>,
    /// The chain the span was last settled for.
    span_for: String,
    /// Step and span diffs keyed older → newer commit id.
    diffs: HashMap<(String, String), Entry>,
    prefetch_seq: u64,
    /// The span whose diff the surface holds.
    shown: Option<(String, String)>,
    pub(super) surface: Entity<DiffSurface>,
    restore_armed: bool,
    acting: bool,
    action_error: Option<String>,
    timeline_scroll: ScrollHandle,
}

impl EvolutionState {
    pub(super) fn new(split: bool, cx: &mut Context<WorkbenchView>) -> Self {
        Self {
            key: None,
            evolog: None,
            versions: Vec::new(),
            error: None,
            seq: 0,
            span: None,
            span_for: String::new(),
            diffs: HashMap::new(),
            prefetch_seq: 0,
            shown: None,
            surface: cx.new(|cx| DiffSurface::new(split, cx)),
            restore_armed: false,
            acting: false,
            action_error: None,
            timeline_scroll: ScrollHandle::new(),
        }
    }
}

fn stamp(version: &EvologEntry) -> &str {
    version
        .op_timestamp
        .as_deref()
        .unwrap_or(&version.timestamp)
}

impl WorkbenchView {
    fn evolution_change(&self, cx: &gpui::App) -> Option<String> {
        self.app.read(cx).ui().evolution.clone()
    }

    /// Follow the model: fetch the chain the tab asks for.
    pub(super) fn sync_evolution(&mut self, cx: &mut Context<Self>) {
        let Some(change) = self.evolution_change(cx) else {
            if self.evolution.key.take().is_some() {
                let state = &mut self.evolution;
                state.seq += 1;
                state.prefetch_seq += 1;
                state.evolog = None;
                state.versions.clear();
                state.error = None;
                state.span = None;
                state.span_for.clear();
                state.diffs.clear();
                state.shown = None;
                state.restore_armed = false;
                state.action_error = None;
            }
            return;
        };
        let key = (change.clone(), self.op_id());
        if self.evolution.key.as_ref() == Some(&key) {
            return;
        }
        let same_change = self
            .evolution
            .key
            .as_ref()
            .is_some_and(|(shown, _)| *shown == change);
        if !same_change {
            self.evolution.evolog = None;
            self.evolution.versions.clear();
            self.evolution.diffs.clear();
            self.evolution.span = None;
            self.evolution.span_for.clear();
            self.evolution.shown = None;
        }
        self.evolution.key = Some(key);
        self.evolution.error = None;
        self.evolution.seq += 1;
        let seq = self.evolution.seq;
        let Some(repo) = self.app.read(cx).repo_path() else {
            return;
        };
        let asked = self.app.read(cx).bg(cx, move |host| {
            host.backend()
                .change_evolog(Path::new(&repo), &change)
                .map_err(CommandError::from)
        });
        cx.spawn(async move |this, cx| {
            let result = asked.await;
            let _ = this.update(cx, |this, cx| {
                if this.evolution.seq != seq {
                    return;
                }
                match result {
                    Ok(evolog) => {
                        // The backend answers newest first.
                        this.evolution.versions = evolog.entries.iter().rev().cloned().collect();
                        this.evolution.evolog = Some(Arc::new(evolog));
                        this.evolution.error = None;
                        this.settle_span(cx);
                        this.prefetch_steps(cx);
                    }
                    Err(error) => {
                        this.evolution.evolog = None;
                        this.evolution.versions.clear();
                        this.evolution.error = Some(error.message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Chain growth (a snapshot, a restore) snaps back to the newest step
    /// — the "what just changed" the pane exists to answer; scrubbing
    /// within one chain keeps the selection.
    fn settle_span(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.evolution;
        let head = state.versions.last().map_or("", |v| v.commit_id.as_str());
        let key = format!(
            "{} {head} {}",
            state.key.as_ref().map_or("", |(change, _)| change.as_str()),
            state.versions.len()
        );
        if key != state.span_for {
            state.span_for = key;
            state.span = default_span(state.versions.len());
        }
        self.span_moved(cx);
    }

    fn key_between(&self, from: usize, to: usize) -> Option<(String, String)> {
        let versions = &self.evolution.versions;
        Some((
            versions.get(from)?.commit_id.clone(),
            versions.get(to)?.commit_id.clone(),
        ))
    }

    fn span_key(&self) -> Option<(String, String)> {
        let span = self.evolution.span?;
        self.key_between(span.from, span.to)
    }

    fn fetch_between(&mut self, key: (String, String), cx: &mut Context<Self>) {
        if self.evolution.diffs.contains_key(&key) {
            return;
        }
        let Some(repo) = self.app.read(cx).repo_path() else {
            return;
        };
        self.evolution.diffs.insert(key.clone(), Entry::Loading);
        let (from, to) = key.clone();
        let asked = self.app.read(cx).bg(cx, move |host| {
            host.backend()
                .compare_diff(Path::new(&repo), &from, &to)
                .map_err(CommandError::from)
                .map(|diff| {
                    // Laid out once, here: the weight is the diff's own
                    // line counts, and the surface shows the same layout.
                    let prepared = Prepared::new(Arc::new(diff));
                    let (added, removed) = prepared
                        .doc()
                        .files
                        .iter()
                        .fold((0, 0), |(a, r), file| (a + file.added, r + file.removed));
                    Held {
                        prepared,
                        added,
                        removed,
                    }
                })
        });
        let seq = self.evolution.seq;
        cx.spawn(async move |this, cx| {
            let result = asked.await;
            let _ = this.update(cx, |this, cx| {
                // A different change took the pane meanwhile.
                if this.evolution.seq != seq && !this.evolution.diffs.contains_key(&key) {
                    return;
                }
                this.evolution.diffs.insert(
                    key,
                    match result {
                        Ok(held) => Entry::Ready(held),
                        Err(error) => Entry::Failed(error.message),
                    },
                );
                this.show_span(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// One request at a time, newest step first: the timeline gets its
    /// per-step weights and scrubbing lands on cache hits.
    fn prefetch_steps(&mut self, cx: &mut Context<Self>) {
        let count = self.evolution.versions.len();
        self.evolution.prefetch_seq += 1;
        if !(2..=PREFETCH_CAP).contains(&count) {
            return;
        }
        let token = self.evolution.prefetch_seq;
        cx.spawn(async move |this, cx| {
            for to in (1..count).rev() {
                loop {
                    let state = this.update(cx, |this, cx| {
                        if this.evolution.prefetch_seq != token {
                            return None;
                        }
                        let key = this.key_between(to - 1, to)?;
                        if !this.evolution.diffs.contains_key(&key) {
                            this.fetch_between(key.clone(), cx);
                        }
                        Some(matches!(
                            this.evolution.diffs.get(&key),
                            Some(Entry::Loading)
                        ))
                    });
                    match state {
                        Ok(Some(true)) => {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(40))
                                .await
                        }
                        Ok(Some(false)) => break,
                        _ => return,
                    }
                }
            }
        })
        .detach();
    }

    /// The span moved: another comparison is another read.
    fn span_moved(&mut self, cx: &mut Context<Self>) {
        self.evolution.restore_armed = false;
        self.evolution.action_error = None;
        if let Some(key) = self.span_key() {
            self.fetch_between(key, cx);
        }
        self.show_span(cx);
        // Keep the focal dot in view while scrubbing a chain wider than
        // the strip: dots and segments alternate in the strip.
        if let Some(span) = self.evolution.span {
            let truncated = self
                .evolution
                .evolog
                .as_ref()
                .is_some_and(|evolog| evolog.truncated);
            self.evolution
                .timeline_scroll
                .scroll_to_item(span.to * 2 + usize::from(truncated));
        }
        cx.notify();
    }

    /// Put the selected span's diff on the surface once it has arrived.
    fn show_span(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.span_key() else {
            return;
        };
        if self.evolution.shown.as_ref() == Some(&key) {
            return;
        }
        let Some(Entry::Ready(held)) = self.evolution.diffs.get(&key) else {
            return;
        };
        let prepared = held.prepared.clone();
        self.evolution
            .surface
            .update(cx, |surface, cx| surface.set_doc(&prepared, false, cx));
        self.evolution.shown = Some(key);
        cx.notify();
    }

    /// Right-click on the version's diff: the diff's reading verbs for the
    /// file under the pointer, as it was in that version — open it, copy
    /// its paths, copy a hunk or the file as a patch. Nothing that changes
    /// the repository: a discard or a move would act on the change as it
    /// is now, with an old version's lines.
    pub(super) fn on_evolution_event(&mut self, event: &DiffEvent, cx: &mut Context<Self>) {
        let DiffEvent::Context { position, target } = event else {
            return;
        };
        let selection = target.selection.clone().unwrap_or_default();
        // A span's diff goes on the surface the moment it is read, so the
        // span's read is what the surface shows; while it is still being
        // read, the surface shows another span's, and names no file.
        let held = self
            .span_key()
            .and_then(|key| match self.evolution.diffs.get(&key) {
                Some(Entry::Ready(held)) => Some(held.prepared.diff().clone()),
                _ => None,
            });
        let file = held.as_ref().and_then(|diff| {
            let path = target.path.as_deref()?;
            diff.files.iter().find(|file| file.path == path)
        });
        let change = self.evolution_change(cx);
        self.app.update(cx, |model, cx| match (file, change) {
            (Some(file), Some(change)) => model.open_diff_menu(
                file,
                &change,
                true,
                target.line(),
                &selection,
                *position,
                cx,
            ),
            _ => model.open_surface_menu(*position, &selection, cx),
        });
    }

    pub(super) fn set_evolution_layout(&mut self, layout: DiffLayout, cx: &mut Context<Self>) {
        self.evolution.surface.update(cx, |surface, cx| {
            surface.set_split(layout == DiffLayout::Split, cx)
        });
    }

    /// ←/→ scrub; ⇧←/⇧→ widen. False when the pane is not up, so the keys
    /// stay free for whatever is.
    pub(super) fn scrub_evolution(
        &mut self,
        delta: isize,
        widen: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.evolution_change(cx).is_none() {
            return false;
        }
        let count = self.evolution.versions.len();
        let Some(span) = self.evolution.span else {
            return true;
        };
        let next = if widen {
            grow(span, delta < 0, count)
        } else {
            scrub(span, delta, count).unwrap_or(span)
        };
        if next != span {
            self.evolution.span = Some(next);
            self.span_moved(cx);
        }
        true
    }

    fn pick_version(&mut self, index: usize, shift: bool, cx: &mut Context<Self>) {
        let count = self.evolution.versions.len();
        self.evolution.span = match (shift, self.evolution.span) {
            (true, Some(span)) => Some(extend_to(span, index, count)),
            _ => step_to(index, count),
        };
        self.span_moved(cx);
    }

    /// The pane's one mutation: the change's tree and description return
    /// to the version under the cursor, as a new undoable operation.
    fn restore_version(&mut self, cx: &mut Context<Self>) {
        let (Some(change), Some(span)) = (self.evolution_change(cx), self.evolution.span) else {
            return;
        };
        let Some(target) = self
            .evolution
            .versions
            .get(span.to)
            .map(|v| v.commit_id.clone())
        else {
            return;
        };
        if self.evolution.acting {
            return;
        }
        self.evolution.acting = true;
        self.evolution.action_error = None;
        cx.notify();
        let task = self.app.update(cx, |model, cx| {
            model.mutate(cx, move |backend, path| {
                backend.restore_change_version(path, &change, &target)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.evolution.acting = false;
                match result {
                    Ok(_) => this.evolution.restore_armed = false,
                    Err(error) => this.evolution.action_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn stats(&self, added: u32, removed: u32, cx: &Context<Self>) -> gpui::Div {
        let t = theme(cx);
        h_flex()
            .gap(px(4.))
            .font_family(t.mono.clone())
            .child(div().text_color(t.add_fg).child(format!("+{added}")))
            .child(
                div()
                    .text_color(t.del_fg)
                    .child(format!("\u{2212}{removed}")),
            )
    }

    fn render_timeline(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let versions = self.evolution.versions.clone();
        let count = versions.len();
        let span = self.evolution.span;
        let mut strip = h_flex()
            .id("evolution-timeline")
            .role(gpui::Role::Group)
            .aria_label("Version timeline")
            .items_end()
            .overflow_x_scroll()
            .track_scroll(&self.evolution.timeline_scroll)
            .px(px(16.))
            .pt(px(8.))
            .pb(px(6.));
        if self
            .evolution
            .evolog
            .as_ref()
            .is_some_and(|evolog| evolog.truncated)
        {
            strip = strip.child(
                div()
                    .id("evolution-older")
                    .flex_shrink_0()
                    .mr(px(4.))
                    .mb(px(16.))
                    .font_family(t.mono.clone())
                    .text_color(t.text3)
                    .tooltip(tooltip(
                        "Older versions exist beyond this list — the walk stopped at its cap",
                    ))
                    .child("⋯"),
            );
        }
        for (index, version) in versions.iter().enumerate() {
            if index > 0 {
                let step = self.key_between(index - 1, index).and_then(|key| {
                    match self.evolution.diffs.get(&key) {
                        Some(Entry::Ready(held)) => {
                            Some((held.prepared.diff().files.len(), held.added, held.removed))
                        }
                        _ => None,
                    }
                });
                let in_span = span.is_some_and(|span| span.from < index && index <= span.to);
                let line = if in_span { t.accent } else { t.border1 };
                // A rewrite that touched no files draws hollow.
                let hollow = matches!(step, Some((0, ..)));
                let mut weight = h_flex().h(px(14.)).justify_center().text_size(px(10.));
                if let Some((files, added, removed)) = step.filter(|(files, ..)| *files > 0) {
                    let _ = files;
                    weight = weight.child(self.stats(added, removed, cx));
                }
                let rewrite = format!(
                    "The rewrite from {} to {}",
                    short_id(&versions[index - 1].commit_id, 8),
                    short_id(&version.commit_id, 8)
                );
                strip = strip.child(
                    v_flex()
                        .id(("evolution-step", index))
                        .role(gpui::Role::Button)
                        .aria_label(rewrite)
                        .aria_selected(in_span)
                        .flex_shrink_0()
                        .w(px(72.))
                        .pb(px(21.))
                        .cursor_pointer()
                        .tooltip(tooltip(format!(
                            "What the rewrite from {} to {} changed",
                            short_id(&versions[index - 1].commit_id, 8),
                            short_id(&version.commit_id, 8).to_owned()
                        )))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.evolution.span = Some(Span {
                                from: index - 1,
                                to: index,
                            });
                            this.span_moved(cx);
                        }))
                        .child(weight)
                        .child(
                            div()
                                .mt(px(3.))
                                .h(px(if in_span { 3. } else { 2. }))
                                .rounded(px(2.))
                                .when(!hollow, |el| el.bg(line))
                                .when(hollow, |el| el.border_1().border_color(line)),
                        ),
                );
            }
            let endpoint = span.is_some_and(|span| index == span.from || index == span.to);
            let focal = span.is_some_and(|span| index == span.to);
            let current = index + 1 == count;
            let size = if focal { 11. } else { 9. };
            let mut dot = div().size(px(size)).rounded(px(size / 2.)).border(px(1.5));
            dot = if focal {
                dot.bg(t.accent).border_color(t.accent)
            } else if endpoint {
                dot.bg(t.bg1).border_color(t.accent)
            } else if current {
                dot.bg(t.working_copy).border_color(t.working_copy)
            } else {
                dot.bg(t.bg1).border_color(t.text3)
            };
            let mut tip = vec![
                format!(
                    "{} · version {} of {count}",
                    short_id(&version.commit_id, 8),
                    index + 1
                ),
                match summary_of(&version.description) {
                    "" => "no description".to_owned(),
                    title => title.to_owned(),
                },
            ];
            if let Some(op) = &version.op_description {
                tip.push(format!("Recorded by: {op}"));
            }
            tip.push(from_now(stamp(version)));
            tip.push("Click to see this version's rewrite · ⇧-click to widen".to_owned());
            strip = strip.child(
                v_flex()
                    .id(("evolution-version", index))
                    .role(gpui::Role::Button)
                    .aria_label(format!(
                        "Version {} of {count}, {}, {}",
                        index + 1,
                        short_id(&version.commit_id, 8),
                        tip[1]
                    ))
                    .aria_description(from_now(stamp(version)))
                    .aria_selected(endpoint)
                    .tabbable(cx)
                    .flex_shrink_0()
                    .items_center()
                    .w(px(58.))
                    .gap(px(4.))
                    .cursor_pointer()
                    .tooltip(tooltip(tip.join("\n")))
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, _, cx| {
                        this.pick_version(index, event.modifiers().shift, cx)
                    }))
                    .child(div().h(px(11.)).flex().items_center().child(dot))
                    .child(
                        div()
                            .font_family(t.mono.clone())
                            .text_size(px(10.))
                            .text_color(if endpoint { t.text1 } else { t.text3 })
                            .child(if endpoint {
                                short_id(&version.commit_id, 8).to_owned()
                            } else {
                                short_age(stamp(version))
                            }),
                    ),
            );
        }
        strip.into_any_element()
    }

    fn render_caption(&mut self, span: Span, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = theme(cx);
        let change = self.evolution_change(cx)?;
        let versions = &self.evolution.versions;
        let (from, focal) = (
            versions.get(span.from)?.clone(),
            versions.get(span.to)?.clone(),
        );
        let count = versions.len();
        let node = find_node(&self.snapshot, &change);
        let can_restore =
            span.to + 1 != count && node.is_some_and(|node| node.kind != NodeKind::Immutable);
        let mut caption = h_flex()
            .gap(px(12.))
            .min_h(px(30.))
            .px(px(16.))
            .pb(px(6.))
            .overflow_hidden()
            .text_size(px(TEXT_S))
            .text_color(t.text3)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .font_family(t.mono.clone())
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text1)
                    .child(format!("v{}", span.to + 1))
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(t.text3)
                            .child(format!("/{count}")),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap(px(6.))
                    .font_family(t.mono.clone())
                    .text_size(px(TEXT_XS))
                    .text_color(t.text2)
                    .child(short_id(&from.commit_id, 8).to_owned())
                    .child(div().text_color(t.text3).child("→"))
                    .child(short_id(&focal.commit_id, 8).to_owned()),
            );
        if span.to - span.from > 1 {
            caption = caption.child(
                div()
                    .flex_shrink_0()
                    .child(format!("{} rewrites as one diff", span.to - span.from)),
            );
        } else if let Some(op) = &focal.op_description {
            caption = caption.child(
                div()
                    .id("evolution-op")
                    .min_w(px(40.))
                    .flex_shrink_1()
                    .tooltip(tooltip(format!(
                        "Recorded by operation {}: {op}",
                        focal.op_id.clone().unwrap_or_default()
                    )))
                    .child(truncated(op.clone())),
            );
        }
        caption = caption.child(
            div()
                .id("evolution-age")
                .flex_shrink_0()
                .tooltip(tooltip(absolute_time(stamp(&focal))))
                .child(from_now(stamp(&focal))),
        );
        if let Some(Entry::Ready(held)) = self
            .span_key()
            .and_then(|key| self.evolution.diffs.get(&key))
        {
            let files = held.prepared.diff().files.len();
            if files > 0 {
                caption = caption.child(
                    h_flex()
                        .flex_shrink_0()
                        .gap(px(6.))
                        .text_size(px(TEXT_XS))
                        .child(format!(
                            "{files} {}",
                            if files == 1 { "file" } else { "files" }
                        ))
                        .child(self.stats(held.added, held.removed, cx)),
                );
            }
        }
        if let Some(error) = &self.evolution.action_error {
            caption = caption.child(
                div()
                    .id("evolution-action-error")
                    .alert(error.clone())
                    .min_w(px(40.))
                    .flex_shrink_1()
                    .text_color(t.danger)
                    .child(truncated(error.clone())),
            );
        }
        caption = caption.child(div().flex_1());
        if can_restore {
            let acting = self.evolution.acting;
            caption = if self.evolution.restore_armed {
                caption
                    .child(
                        div()
                            .min_w(px(40.))
                            .flex_shrink_1()
                            .text_color(t.text2)
                            .child(truncated(format!(
                                "Files and description return to {}; descendants rebase. Undoable \
                             from the op log.",
                                short_id(&focal.commit_id, 8)
                            ))),
                    )
                    .child(
                        btn(
                            "evolution-restore-go",
                            if acting { "Restoring…" } else { "Restore" },
                        )
                        .small()
                        .primary()
                        .disabled(acting)
                        .on_click(cx.listener(|this, _, _, cx| this.restore_version(cx))),
                    )
                    .child(
                        btn("evolution-restore-cancel", "Cancel")
                            .small()
                            .disabled(acting)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.evolution.restore_armed = false;
                                cx.notify();
                            })),
                    )
            } else {
                caption.child(
                    btn("evolution-restore", "Restore this version")
                        .small()
                        .icon(IconName::Restore)
                        .disabled(acting)
                        .tooltip(format!(
                            "Restore {} to version {}",
                            short_id(&change, 4),
                            short_id(&focal.commit_id, 8)
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.evolution.restore_armed = true;
                            cx.notify();
                        })),
                )
            };
        }
        Some(caption.into_any_element())
    }

    fn render_evolution_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = theme(cx);
        let note = |text: String, color| {
            div()
                .py(px(12.))
                .px(px(16.))
                .text_size(px(TEXT_S))
                .text_color(color)
                .child(text)
        };
        let skeleton = || {
            let mut skeleton = v_flex().gap(px(9.)).p(px(16.));
            for width in [1., 0.86, 0.64, 0.78, 0.42] {
                skeleton = skeleton.child(
                    div()
                        .h(px(12.))
                        .w(gpui::relative(width))
                        .rounded(px(4.))
                        .bg(t.bg3),
                );
            }
            crate::ui::motion::breathing("evolution-skeleton", skeleton).into_any_element()
        };
        if let Some(error) = &self.evolution.error {
            return note(error.clone(), t.warn)
                .id("evolution-error")
                .alert(error.clone())
                .into_any_element();
        }
        if self.evolution.evolog.is_none() {
            return skeleton();
        }
        if self.evolution.versions.len() == 1 {
            return div()
                .size_full()
                .child(empty_state(
                    IconName::Operations,
                    "No earlier versions",
                    "This change has never been rewritten — its whole life is the diff you were \
                     just reading.",
                    None,
                    cx,
                ))
                .into_any_element();
        }
        let key = self.span_key();
        let held = key.as_ref().and_then(|key| self.evolution.diffs.get(key));
        match held {
            None | Some(Entry::Loading) => skeleton(),
            Some(Entry::Failed(error)) => note(error.clone(), t.warn)
                .id("evolution-step-error")
                .alert(error.clone())
                .into_any_element(),
            Some(Entry::Ready(held)) if held.prepared.diff().files.is_empty() => {
                let span = self.evolution.span;
                let pair = span.and_then(|span| {
                    let versions = &self.evolution.versions;
                    Some((versions.get(span.from)?, versions.get(span.to)?))
                });
                match pair.filter(|(from, to)| from.description != to.description) {
                    Some((from, to)) => {
                        let side = |tag: &'static str, text: &str, fg, bg| {
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(6.))
                                .p(px(12.))
                                .rounded(px(6.))
                                .bg(bg)
                                .child(
                                    div()
                                        .text_size(px(TEXT_XS))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(fg)
                                        .child(tag),
                                )
                                .child(
                                    div()
                                        .font_family(t.mono.clone())
                                        .text_size(px(12.))
                                        .line_height(gpui::relative(1.5))
                                        .text_color(t.text1)
                                        .child(if text.is_empty() {
                                            "(no description)".to_owned()
                                        } else {
                                            text.to_owned()
                                        }),
                                )
                        };
                        v_flex()
                            .p(px(16.))
                            .gap(px(10.))
                            .child(
                                div().text_size(px(TEXT_S)).text_color(t.text3).child(
                                    "This rewrite changed no files — the description moved:",
                                ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(12.))
                                    .child(side("before", &from.description, t.del_fg, t.del_bg))
                                    .child(side("after", &to.description, t.add_fg, t.add_bg)),
                            )
                            .into_any_element()
                    }
                    None => div()
                        .size_full()
                        .child(empty_state(
                            IconName::Commit,
                            "No file changes",
                            "Nothing differs between these versions' trees or descriptions — \
                             this rewrite only moved the change (a rebase, or a snapshot that \
                             touched nothing here).",
                            None,
                            cx,
                        ))
                        .into_any_element(),
                }
            }
            Some(Entry::Ready(held)) => {
                let mut body = v_flex().size_full().child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(self.evolution.surface.clone()),
                );
                if held.prepared.diff().truncated {
                    body = body.child(
                        note(
                            "File list truncated — this rewrite touched more files than Jiji \
                             renders"
                                .to_owned(),
                            t.text3,
                        )
                        .border_t_1()
                        .border_color(t.border2),
                    );
                }
                body.into_any_element()
            }
        }
    }

    pub(super) fn render_evolution(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = theme(cx);
        let change = self.evolution_change(cx)?;
        let (lead, rest) = change.split_at(change.len().min(2));
        let app = self.app.clone();
        let head = h_flex()
            .flex_shrink_0()
            .min_h(px(34.))
            .gap(px(8.))
            .pl(px(16.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border2)
            .child(icon(IconName::Operations, 13., t.text3))
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_S))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text1)
                    .child("Evolution"),
            )
            .child(
                label_box(t.text2, t.border1)
                    .id("evolution-of")
                    .gap(px(4.))
                    .font_family(t.mono.clone())
                    .tooltip(tooltip(
                        "Every version this change was rewritten through, oldest to newest",
                    ))
                    .child("of")
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text1)
                                    .child(lead.to_owned()),
                            )
                            .child(rest.chars().take(6).collect::<String>()),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(TEXT_XS))
                    .text_color(t.text3)
                    .child(truncated(
                        "←/→ scrub through time · ⇧ widens the span · click any dot",
                    )),
            )
            .child(
                icon_btn("evolution-close", IconName::Close)
                    .size(24., 13.)
                    .tooltip("Back to the diff (esc)")
                    .on_click(move |_, _, cx| {
                        app.update(cx, |model, cx| model.close_evolution(cx))
                    }),
            );
        let mut pane = v_flex()
            .id(SharedString::from("evolution"))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(head);
        if self.evolution.evolog.is_some() && !self.evolution.versions.is_empty() {
            let mut strip = v_flex()
                .flex_shrink_0()
                .border_b_1()
                .border_color(t.border2)
                .bg(t.bg0)
                .child(self.render_timeline(cx));
            if let Some(span) = self.evolution.span {
                strip = strip.children(self.render_caption(span, cx));
            }
            pane = pane.child(strip);
        }
        Some(
            pane.child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_evolution_body(cx)),
            )
            .into_any_element(),
        )
    }
}

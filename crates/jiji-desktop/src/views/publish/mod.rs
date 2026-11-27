//! Publish: which GitHub repo this jj repo publishes to, and the workflows
//! on top of that connection — publish a stack, land it (by hand or as a
//! supervised auto-land job), ship it straight to trunk, and fetch any
//! pull request for local review. Every workflow has the same shape:
//! plan (derived by jiji-forge), confirm, execute.

mod review;
mod stacks;

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, Anchor, AnyElement, Context, Entity, FocusHandle, Focusable, FontWeight,
    Hsla, ScrollHandle, SharedString, Subscription, Window,
};
use jiji_core::snapshot::RepoSnapshot;
use jiji_forge::{
    AutoLandStatus, LandOptions, LandOutcome, LandPlan, MergeMethod, PrSummary, PublishStack,
    RepoPrState, ShipOutcome, ShipPlan, StackLink, SubmitHead, SubmitOutcome, SubmitPlan,
    SubmitStepStatus, TakeOverOutcome, TakeOverPlan, TokenSource,
};

use crate::app::{app, AppEvent, AppModel, ForgePhase, UiIntent};
use crate::logic::autoland::{is_interrupted, live_job, resume_blocker, story, visible_jobs};
use crate::logic::publish::{
    autoland_options_story, job_title, job_tone, land_action_text, review_rows,
};
use crate::shell::Dismiss;
use crate::theme::{theme, TEXT_M, TEXT_S, TEXT_XS};
use crate::ui::{
    btn, dot, empty_state, h_flex, icon, icon_btn, input::field, popover, popover_layer, ref_label,
    segmented, v_flex, IconName, InputEvent, NodeExt as _, PopoverState, Segment, Tabbable as _,
    TextInput,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Stacks,
    Review,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Land,
    Publish,
    Ship,
}

pub struct PublishView {
    app: Entity<AppModel>,
    focus: FocusHandle,

    /// The complete local stacks, read without changing the graph's lens,
    /// and the snapshot they were discovered in.
    snapshot: Option<Arc<RepoSnapshot>>,
    stacks: Vec<PublishStack>,
    stacks_loading: bool,
    stacks_error: Option<String>,
    stacks_seq: u64,
    /// What the stacks were last read for: the app's snapshot and PR state.
    stacks_for: Option<(usize, usize)>,

    token: Entity<TextInput>,
    connecting: bool,
    /// Connect/disconnect refusals; connection-level failures from the
    /// shared sync arrive via the model.
    action_error: Option<String>,
    connection: PopoverState,

    page: Page,
    selected: Option<String>,
    query: Entity<TextInput>,
    show_local: bool,
    mode: Mode,
    more: PopoverState,
    /// Disclosures the user opened, by name.
    open: HashSet<String>,

    plan_head: Option<SubmitHead>,
    plan: Option<SubmitPlan>,
    plan_loading: bool,
    plan_error: Option<String>,
    publishing: bool,
    outcome: Option<SubmitOutcome>,
    plan_seq: u64,

    land_for: Option<String>,
    land: Option<LandPlan>,
    land_loading: bool,
    land_error: Option<String>,
    landing: bool,
    land_outcome: Option<LandOutcome>,
    land_seq: u64,
    /// The run's user-steerable rules. Changing either re-plans: the
    /// options shape the plan itself.
    land_method: Option<MergeMethod>,
    land_bypass: bool,
    /// What the next publish does about GitHub's own stack. Changing it
    /// re-plans too.
    stack_link: StackLink,

    ship_for: Option<String>,
    ship: Option<ShipPlan>,
    ship_loading: bool,
    ship_error: Option<String>,
    shipping: bool,
    ship_outcome: Option<ShipOutcome>,
    ship_seq: u64,

    queueing: bool,
    queue_error: Option<String>,
    resuming: bool,
    resume_error: Option<String>,

    /// The pull request the user asked to take over, or to give back, and
    /// the plan for it once it is in. The plan is shown in the action
    /// column and runs only when confirmed there.
    takeover_for: Option<u64>,
    takeover: Option<TakeOverPlan>,
    takeover_loading: bool,
    takeover_error: Option<String>,
    taking_over: bool,
    takeover_outcome: Option<TakeOverOutcome>,
    takeover_seq: u64,
    /// The stack to show once the stacks have been read again: the one a
    /// take-over just put their pull request in.
    follow: Option<String>,

    review_for: Option<PrSummary>,
    review_name: Entity<TextInput>,
    review_busy: bool,
    review_error: Option<String>,
    /// The bookmark the last fetch landed in, and the change it points at.
    review_done: Option<(String, Option<String>)>,
    lookup: Entity<TextInput>,
    lookup_busy: bool,
    lookup_error: Option<String>,
    rerun_busy: Option<u64>,
    rerun_note: Option<String>,

    list_scroll: ScrollHandle,
    chain_scroll: ScrollHandle,
    plan_scroll: ScrollHandle,
    review_scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for PublishView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

gpui::actions!(publish, [FindStack]);

pub fn bind_keys(cx: &mut gpui::App) {
    cx.bind_keys([gpui::KeyBinding::new(
        "/",
        FindStack,
        Some("Publish && !TextInput"),
    )]);
}

fn token_source_label(source: TokenSource) -> &'static str {
    match source {
        TokenSource::Keychain => "stored in your keychain",
        TokenSource::Environment => "from GITHUB_TOKEN",
        TokenSource::GhCli => "from the gh CLI",
    }
}

/// What a notice's link does.
type NoticeAction = Box<dyn Fn(&mut PublishView, &mut Window, &mut Context<PublishView>)>;

impl PublishView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let app = app(cx);
        let token = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder("github_pat_…")
                .label("GitHub personal access token")
                .masked()
        });
        let query = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder("Find a stack…")
                .label("Find a stack")
        });
        let review_name = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder("Local bookmark")
                .label("Local bookmark name")
        });
        let lookup = cx.new(|cx| {
            TextInput::new(window, cx)
                .placeholder("PR number")
                .label("Pull request number")
        });
        let subscriptions = vec![
            cx.observe_in(&app, window, |this, _, _, cx| {
                this.sync(cx);
                cx.notify();
            }),
            cx.subscribe_in(&app, window, |this, _, event: &AppEvent, _, cx| {
                if *event == AppEvent::Intent {
                    this.take_intents(cx);
                }
            }),
            cx.subscribe_in(
                &token,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Submit => this.connect(cx),
                    _ => cx.notify(),
                },
            ),
            cx.subscribe_in(&query, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(
                &review_name,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Submit => this.fetch_for_review(cx),
                    InputEvent::Cancel => this.close_review_panel(cx),
                    _ => cx.notify(),
                },
            ),
            cx.subscribe_in(
                &lookup,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Submit => this.lookup_pr(window, cx),
                    _ => cx.notify(),
                },
            ),
        ];
        let mut this = Self {
            app,
            focus: cx.focus_handle(),
            snapshot: None,
            stacks: Vec::new(),
            stacks_loading: true,
            stacks_error: None,
            stacks_seq: 0,
            stacks_for: None,
            token,
            connecting: false,
            action_error: None,
            connection: PopoverState::new(cx),
            page: Page::Stacks,
            selected: None,
            query,
            show_local: false,
            mode: Mode::Land,
            more: PopoverState::new(cx),
            open: HashSet::new(),
            plan_head: None,
            plan: None,
            plan_loading: false,
            plan_error: None,
            publishing: false,
            outcome: None,
            plan_seq: 0,
            land_for: None,
            land: None,
            land_loading: false,
            land_error: None,
            landing: false,
            land_outcome: None,
            land_seq: 0,
            land_method: None,
            land_bypass: false,
            stack_link: StackLink::Follow,
            ship_for: None,
            ship: None,
            ship_loading: false,
            ship_error: None,
            shipping: false,
            ship_outcome: None,
            ship_seq: 0,
            queueing: false,
            queue_error: None,
            resuming: false,
            resume_error: None,
            takeover_for: None,
            takeover: None,
            takeover_loading: false,
            takeover_error: None,
            taking_over: false,
            takeover_outcome: None,
            takeover_seq: 0,
            follow: None,
            review_for: None,
            review_name,
            review_busy: false,
            review_error: None,
            review_done: None,
            lookup,
            lookup_busy: false,
            lookup_error: None,
            rerun_busy: None,
            rerun_note: None,
            list_scroll: ScrollHandle::new(),
            chain_scroll: ScrollHandle::new(),
            plan_scroll: ScrollHandle::new(),
            review_scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        this.sync(cx);
        this
    }

    // ── What the model says ──

    fn repo_path(&self, cx: &gpui::App) -> Option<String> {
        self.app.read(cx).repo_path()
    }

    /// A detected repo and a token verified this session.
    fn connected(&self, cx: &gpui::App) -> bool {
        self.app
            .read(cx)
            .forge
            .status
            .as_ref()
            .is_some_and(|status| status.auth.source.is_some() && status.auth.login.is_some())
    }

    fn executing(&self) -> bool {
        self.publishing || self.landing || self.shipping || self.queueing || self.taking_over
    }

    fn selected_stack(&self) -> Option<&PublishStack> {
        let key = self.selected.as_deref()?;
        self.stacks.iter().find(|stack| stack.key == key)
    }

    /// The remote every plan of this repo uses; job states don't carry it.
    fn job_remote(&self, cx: &gpui::App) -> String {
        self.app
            .read(cx)
            .forge
            .status
            .as_ref()
            .and_then(|status| status.repo.as_ref())
            .map_or_else(|| "origin".to_owned(), |repo| repo.remote.clone())
    }

    /// A running job owns the queue affordance — repo-wide, since the host
    /// runs one live job at a time.
    fn job_active(&self, cx: &gpui::App) -> bool {
        live_job(&self.app.read(cx).autoland.jobs).is_some()
    }

    /// Follow the model: re-read the stacks when the snapshot or the PR
    /// state moved, keep a stack selected, and take pending intents.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let (snapshot, prs) = {
            let model = self.app.read(cx);
            (model.snapshot().cloned(), model.forge.prs.clone())
        };
        let Some(snapshot) = snapshot else {
            return;
        };
        let key = (
            Arc::as_ptr(&snapshot) as usize,
            prs.as_ref().map_or(0, |prs| Arc::as_ptr(prs) as usize),
        );
        if self.stacks_for != Some(key) {
            self.stacks_for = Some(key);
            self.load_stacks(snapshot.repo_path.clone(), prs, cx);
        }
        self.settle_selection(cx);
        self.take_intents(cx);
    }

    fn load_stacks(&mut self, repo: String, prs: Option<Arc<RepoPrState>>, cx: &mut Context<Self>) {
        self.stacks_seq += 1;
        let seq = self.stacks_seq;
        self.stacks_loading = true;
        self.stacks_error = None;
        let read = self
            .app
            .read(cx)
            .bg(cx, move |host| host.publish_stacks(&repo, prs.as_deref()));
        cx.spawn(async move |this, cx| {
            let result = read.await;
            let _ = this.update(cx, |this, cx| {
                if this.stacks_seq != seq {
                    return;
                }
                this.stacks_loading = false;
                match result {
                    Ok((snapshot, stacks)) => {
                        // A stack published from its head change comes
                        // back under its new bookmark's key: follow it, so
                        // what was just done stays on screen.
                        let head = this
                            .selected_stack()
                            .and_then(|stack| stack.head_change.clone());
                        this.snapshot = Some(snapshot);
                        this.stacks = stacks;
                        // A take-over put their pull request in a stack
                        // of its own key: that is the one to show.
                        if let Some(key) = this
                            .follow
                            .take_if(|key| this.stacks.iter().any(|stack| stack.key == *key))
                        {
                            this.select_stack(&key, cx);
                        }
                        if this.selected_stack().is_none() {
                            if let Some(stack) = this
                                .stacks
                                .iter()
                                .find(|stack| head.is_some() && stack.head_change == head)
                            {
                                this.selected = Some(stack.key.clone());
                            }
                        }
                    }
                    Err(error) => this.stacks_error = Some(error.message),
                }
                this.settle_selection(cx);
                this.take_intents(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Open the current published stack immediately, and keep the
    /// selection as facts refresh: a PR fetch must not steal the user's
    /// place in the list.
    fn settle_selection(&mut self, cx: &mut Context<Self>) {
        let (ready, connected) = {
            let forge = &self.app.read(cx).forge;
            (
                forge.phase == ForgePhase::Ready && !forge.prs_loading && !self.stacks_loading,
                self.connected(cx),
            )
        };
        if !ready || self.stacks.is_empty() {
            return;
        }
        let current = self.selected_stack().cloned();
        match current {
            None => {
                let pick = self
                    .stacks
                    .iter()
                    .find(|stack| stack.is_active)
                    .unwrap_or(&self.stacks[0])
                    .key
                    .clone();
                self.select_stack(&pick, cx);
            }
            Some(stack) => {
                let idle = self.plan.is_none()
                    && self.land.is_none()
                    && self.ship.is_none()
                    && !self.plan_loading
                    && !self.land_loading
                    && !self.ship_loading
                    && self.plan_error.is_none()
                    && self.land_error.is_none()
                    && self.ship_error.is_none()
                    && self.outcome.is_none()
                    && self.land_outcome.is_none()
                    && self.ship_outcome.is_none()
                    && self.takeover_for.is_none();
                if connected && idle && stack.submit_head.is_some() {
                    self.select_stack(&stack.key, cx);
                }
            }
        }
    }

    /// The palette's "Land <bookmark>…" and "Ship … to trunk…" rows land
    /// here: open the stack's plan card, the same as clicking its row.
    fn take_intents(&mut self, cx: &mut Context<Self>) {
        if self.stacks_loading {
            return;
        }
        let intent = self.app.update(cx, |model, _| {
            model.take_intent(|intent| {
                matches!(
                    intent,
                    UiIntent::Land { .. }
                        | UiIntent::Ship { .. }
                        | UiIntent::TakeOver { .. }
                        | UiIntent::GiveBack { .. }
                )
            })
        });
        match intent {
            // The palette's "Take over #9…" and "Give back #9…": the
            // stack the pull request is on, with the plan to confirm.
            Some(UiIntent::TakeOver { number }) => {
                let on = self
                    .stacks
                    .iter()
                    .find(|stack| stack.others.iter().any(|other| other.number == number))
                    .map(|stack| stack.key.clone());
                if let Some(key) = on {
                    self.select_stack(&key, cx);
                }
                self.page = Page::Stacks;
                self.ask_take_over(number, cx);
            }
            Some(UiIntent::GiveBack { number }) => {
                let on = self
                    .stacks
                    .iter()
                    .find(|stack| {
                        stack.segments.iter().any(|segment| {
                            segment
                                .taken_over
                                .as_ref()
                                .is_some_and(|taken| taken.number == number)
                        })
                    })
                    .map(|stack| stack.key.clone());
                if let Some(key) = on {
                    self.select_stack(&key, cx);
                }
                self.page = Page::Stacks;
                self.ask_give_back(number, cx);
            }
            Some(UiIntent::Land { bookmark }) => {
                self.selected = self
                    .stacks
                    .iter()
                    .find(|stack| stack.head_bookmark.as_deref() == Some(&bookmark))
                    .map(|stack| stack.key.clone());
                self.page = Page::Stacks;
                self.load_land_plan(bookmark, cx);
            }
            Some(UiIntent::Ship { head }) => {
                self.selected = self
                    .stacks
                    .iter()
                    .find(|stack| stack.head_change.as_deref() == Some(&head))
                    .map(|stack| stack.key.clone());
                self.page = Page::Stacks;
                self.load_ship_plan(head, cx);
            }
            _ => {}
        }
    }

    // ── Connection ──

    fn connect(&mut self, cx: &mut Context<Self>) {
        let token = self.token.read(cx).text().trim().to_owned();
        if token.is_empty() || self.connecting {
            return;
        }
        self.connecting = true;
        self.action_error = None;
        cx.notify();
        let task = self
            .app
            .update(cx, |model, cx| model.connect_forge(token, cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.connecting = false;
                match result {
                    Ok(()) => this.token.update(cx, |input, cx| input.set_text("", cx)),
                    Err(error) => this.action_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        self.connecting = true;
        self.action_error = None;
        cx.notify();
        let task = self.app.update(cx, |model, cx| model.disconnect_forge(cx));
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.connecting = false;
                if let Err(error) = result {
                    this.action_error = Some(error.message);
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ── Auto-land ──

    /// Queue the planned stack for auto-land: the supervised job takes
    /// over from here — its card replaces the manual plan flow, and
    /// re-deriving plans is its whole point, so the confirmed plan object
    /// is not sent.
    fn queue_autoland(&mut self, cx: &mut Context<Self>) {
        let (Some(plan), Some(head)) = (&self.land, self.land_for.clone()) else {
            return;
        };
        if self.queueing || self.landing || !crate::logic::autoland::can_queue(plan) {
            return;
        }
        self.queueing = true;
        self.queue_error = None;
        cx.notify();
        let options = self.land_options();
        let task = self.app.update(cx, |model, cx| {
            model.start_autoland(head, Some(options), cx)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.queueing = false;
                match result {
                    Ok(()) => this.land_outcome = None,
                    Err(error) => this.queue_error = Some(error.message),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Resuming an interrupted job is the same call queueing makes — the
    /// backend seeds the engine from the surviving record. The visibly
    /// confirmed saved rules are re-sent: an admin bypass must never be a
    /// hidden consequence of a generic resume action.
    fn resume_autoland(&mut self, head: String, options: LandOptions, cx: &mut Context<Self>) {
        if self.resuming {
            return;
        }
        self.resuming = true;
        self.resume_error = None;
        cx.notify();
        let task = self.app.update(cx, |model, cx| {
            model.start_autoland(head, Some(options), cx)
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.resuming = false;
                if let Err(error) = result {
                    this.resume_error = Some(error.message);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn land_options(&self) -> LandOptions {
        LandOptions {
            method: self.land_method,
            bypass: self.land_bypass,
        }
    }

    // ── Shared pieces ──

    fn toggle_open(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.open.remove(name) {
            self.open.insert(name.to_owned());
        }
        cx.notify();
    }

    /// A collapsible section of a plan card: a summary line that opens
    /// what is behind it.
    fn disclosure(
        &self,
        name: &str,
        label: &'static str,
        aside: Option<String>,
        content: impl FnOnce(&mut Context<Self>) -> AnyElement,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let t = theme(cx);
        let open = self.open.contains(name);
        let toggle = name.to_owned();
        let mut section = v_flex()
            .py(px(4.))
            .border_t_1()
            .border_color(t.border2)
            .text_size(px(TEXT_S))
            .child(
                h_flex()
                    .id(SharedString::from(format!("disclosure-{name}")))
                    .role(gpui::Role::Button)
                    .aria_label(match &aside {
                        Some(aside) => format!("{label}, {aside}"),
                        None => label.to_owned(),
                    })
                    .aria_expanded(open)
                    .tabbable(cx)
                    .gap(px(6.))
                    .h(px(26.))
                    .cursor_pointer()
                    .text_color(t.text2)
                    .hover(|style| style.text_color(t.text1))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_open(&toggle, cx)))
                    .child(icon(
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        11.,
                        t.text3,
                    ))
                    .child(label)
                    .child(div().flex_1())
                    .children(aside.map(|aside| div().text_color(t.text3).child(aside))),
            );
        if open {
            section = section.child(content(cx));
        }
        section
    }

    /// The per-step results of an executed plan.
    fn result_steps(
        &self,
        steps: Vec<(SubmitStepStatus, String, Option<String>)>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let mut list = v_flex().text_size(px(TEXT_S)).text_color(t.text1);
        for (status, text, detail) in steps {
            let (glyph, color) = match status {
                SubmitStepStatus::Done => ("✓", t.ok),
                SubmitStepStatus::Failed => ("×", t.danger),
                SubmitStepStatus::Skipped => ("–", t.text3),
            };
            let mut body = v_flex().flex_1().min_w_0().child(text);
            if let Some(detail) = detail {
                body = body.child(div().text_color(t.text3).child(detail));
            }
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(8.))
                    .py(px(3.))
                    .line_height(gpui::relative(1.5))
                    .child(
                        div()
                            .w(px(14.))
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .font_family(t.mono.clone())
                            .text_color(color)
                            .child(glyph),
                    )
                    .child(body),
            );
        }
        list.into_any_element()
    }

    /// A line across the page: something is off, and what to do about it.
    fn notice(
        &self,
        id: &'static str,
        text: String,
        error: bool,
        action: Option<(&'static str, NoticeAction)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let mut line = h_flex()
            .gap(px(12.))
            .flex_shrink_0()
            .min_h(px(30.))
            .py(px(4.))
            .px(px(16.))
            .border_b_1()
            .border_color(t.border2)
            .text_size(px(TEXT_S))
            .text_color(if error { t.danger } else { t.text2 })
            .child(
                div()
                    .id(id)
                    .map(|el| {
                        if error {
                            el.alert(text.clone())
                        } else {
                            el.status(text.clone())
                        }
                    })
                    .flex_1()
                    .min_w_0()
                    .child(text),
            );
        if let Some((label, run)) = action {
            line = line.child(self.text_button(
                (id, 1usize),
                label,
                move |this, window, cx| run(this, window, cx),
                cx,
            ));
        }
        line.into_any_element()
    }

    fn text_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        run: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let label = label.into();
        div()
            .id(id.into())
            .role(gpui::Role::Button)
            .aria_label(label.trim_end_matches(['↗', ' ', '…']).to_owned())
            .tabbable(cx)
            .flex_shrink_0()
            .cursor_pointer()
            .text_size(px(TEXT_S))
            .text_color(t.accent_strong)
            .hover(|style| style.text_color(t.text1))
            .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
            .child(label)
            .into_any_element()
    }

    /// Retry after a failed stack read: read the stacks again now, as well
    /// as the rest. The snapshot and the PR state may never move again to
    /// do it for us.
    fn retry_stacks(&mut self, cx: &mut Context<Self>) {
        self.stacks_for = None;
        self.sync(cx);
        self.refresh_selected(cx);
    }

    fn refresh_selected(&mut self, cx: &mut Context<Self>) {
        self.app.update(cx, |model, cx| model.refresh_forge_prs(cx));
        let Some(stack) = self.selected_stack().cloned() else {
            return;
        };
        if self.executing() {
            return;
        }
        match self.mode {
            Mode::Land => {
                if let Some(bookmark) = stack.head_bookmark {
                    self.load_land_plan(bookmark, cx);
                }
            }
            Mode::Publish => {
                if let Some(head) = stack.submit_head {
                    self.load_plan(head, cx);
                }
            }
            Mode::Ship => {
                if let Some(head) = stack.head_change {
                    self.load_ship_plan(head, cx);
                }
            }
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connection.is_open() || self.more.is_open() {
            self.connection.close(window, cx);
            self.more.close(window, cx);
            cx.notify();
        } else if self.review_for.is_some() {
            self.close_review_panel(cx);
        } else {
            cx.propagate();
        }
    }

    // ── The page ──

    /// The section's one toolbar, the same height and hairline as the
    /// workbench's: what this is, which view, and the connection.
    fn render_page_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let (repo, auth, prs_loading, review_count) = {
            let forge = &self.app.read(cx).forge;
            let repo = forge.status.as_ref().and_then(|status| status.repo.clone());
            let count = review_rows(
                forge.prs.as_deref(),
                repo.as_ref().map(|repo| repo.owner.as_str()),
            )
            .len();
            (
                repo,
                forge.status.as_ref().map(|status| status.auth.clone()),
                forge.prs_loading,
                count,
            )
        };
        let connected = self.connected(cx);
        let published = self
            .stacks
            .iter()
            .filter(|stack| stack.pr_count > 0)
            .count();
        let stack_count = if published > 0 {
            published
        } else {
            self.stacks.len()
        };
        let repo_label = match (&repo, &self.snapshot) {
            (Some(repo), _) => format!("{}/{}", repo.owner, repo.name),
            (None, Some(snapshot)) => snapshot.repo_name.clone(),
            (None, None) => String::new(),
        };
        let view = cx.entity().downgrade();
        let tabs = segmented(
            "publish-pages",
            "Publish views",
            vec![
                Segment::text(Page::Stacks, format!("Stacks  {stack_count}")),
                Segment::text(Page::Review, format!("Pull requests  {review_count}")),
            ],
            &self.page,
            cx,
            move |page, _, cx| {
                let page = *page;
                let _ = view.update(cx, |this, cx| {
                    this.page = page;
                    cx.notify();
                });
            },
        );
        let login = auth.as_ref().and_then(|auth| auth.login.clone());
        let mut connection = div().relative().child(
            btn(
                "publish-connection",
                if connected {
                    format!("@{}", login.unwrap_or_default())
                } else {
                    "Connect GitHub".to_owned()
                },
            )
            .ghost()
            .on(self.connection.is_open())
            .expanded(self.connection.is_open())
            .tooltip("GitHub connection")
            .trailing(icon(IconName::ChevronDown, 12., t.text3))
            .on_click(cx.listener(|this, _, window, cx| {
                this.connection.toggle(window, cx);
                cx.notify();
            })),
        );
        if self.connection.is_open() {
            let panel = self.render_connection(window, cx);
            connection = connection.child(popover_layer(
                Anchor::TopRight,
                30.,
                self.connection.panel(
                    "connection-panel",
                    gpui::Role::Dialog,
                    "GitHub connection",
                    panel,
                ),
                cx.listener(|this, _, window, cx| {
                    this.connection.close(window, cx);
                    cx.notify();
                }),
            ));
        }
        let section = crate::app::Section::Publish;
        crate::ui::page_bar_marked(
            Some((section.glyph(), section.hue(&t))),
            "Publish",
            None,
            cx,
        )
        .gap(px(8.))
        .child(
            div()
                .min_w(px(40.))
                .flex_shrink_1()
                .font_family(t.mono.clone())
                .text_size(px(TEXT_S))
                .text_color(t.text3)
                .child(crate::ui::truncated(repo_label)),
        )
        .child(div().flex_shrink_0().ml(px(8.)).child(tabs))
        .child(div().flex_1())
        .children(repo.as_ref().map(|repo| {
            div()
                .flex_shrink_0()
                .mr(px(4.))
                .text_size(px(TEXT_XS))
                .text_color(t.text3)
                .child(format!("via {}", repo.remote))
        }))
        .child(
            icon_btn("publish-refresh", IconName::Refresh)
                .disabled(prs_loading || self.executing())
                .tooltip("Refresh pull requests and selected plan")
                .on_click(cx.listener(|this, _, _, cx| this.refresh_selected(cx))),
        )
        .child(
            h_flex()
                .gap(px(6.))
                .child(dot(if connected { t.ok } else { t.text3 }, 6.))
                .child(connection),
        )
    }

    fn render_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let t = theme(cx);
        let (phase, auth, forge_error) = {
            let forge = &self.app.read(cx).forge;
            (
                forge.phase,
                forge.status.as_ref().map(|status| status.auth.clone()),
                forge.error.clone(),
            )
        };
        let connected = self.connected(cx);
        let busy = self.connecting;
        let strong = |text: &'static str| {
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.text1)
                .child(text)
        };
        let body = |text: String| div().mt(px(4.)).mb(px(10.)).text_color(t.text2).child(text);
        let mut panel = popover(cx).w(px(330.)).p(px(16.)).text_size(px(TEXT_S));
        panel = if matches!(
            phase,
            ForgePhase::Idle | ForgePhase::Loading | ForgePhase::Verifying
        ) {
            panel.child(div().text_color(t.text2).child("Checking GitHub…"))
        } else if let (true, Some(source)) = (connected, auth.and_then(|auth| auth.source)) {
            let panel = panel
                .child(strong("Connected to GitHub"))
                .child(body(format!("Token {}.", token_source_label(source))));
            if source == TokenSource::Keychain {
                panel.child(
                    h_flex().child(
                        btn("forge-disconnect", "Disconnect")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                    ),
                )
            } else {
                panel.child(div().text_color(t.text3).child("Managed outside Jiji"))
            }
        } else {
            let empty = self.token.read(cx).text().trim().is_empty();
            panel
                .child(strong("Connect to GitHub"))
                .child(body(
                    "Use your gh CLI login or a personal access token.".to_owned(),
                ))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .child(field(&self.token, window, cx).flex_1())
                        .child(
                            btn(
                                "forge-connect",
                                if busy { "Connecting…" } else { "Connect" },
                            )
                            .primary()
                            .disabled(busy || empty)
                            .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                        ),
                )
                .child(div().mt(px(10.)).child(self.text_button(
                    "forge-create-token",
                    "Create a token ↗",
                    |_, _, cx| cx.open_url("https://github.com/settings/tokens"),
                    cx,
                )))
        };
        if let Some(error) = self.action_error.clone().or(forge_error) {
            panel = panel.child(
                div()
                    .id("connection-error")
                    .alert(error.clone())
                    .mt(px(10.))
                    .text_color(t.danger)
                    .child(error),
            );
        }
        panel
    }

    /// Auto-land jobs: one flat band per record, the live job first. The
    /// status bar carries the same story as a chip; this is where it is
    /// acted on.
    fn render_job(
        &self,
        status: &AutoLandStatus,
        snapshot: &RepoSnapshot,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme(cx);
        let job = &status.record.state;
        let interrupted = is_interrupted(status);
        let terminal = job.phase.is_terminal();
        let blocker = interrupted
            .then(|| resume_blocker(status, &snapshot.bookmarks))
            .flatten();
        let color = job_tone(status).map_or(t.accent_strong, |tone| t.tone(tone));
        let job_active = self.job_active(cx);
        let stopping = self.app.read(cx).autoland.stopping;
        let dismiss = |cx: &mut Context<Self>| {
            let status = status.clone();
            let app = self.app.clone();
            btn(
                SharedString::from(format!("job-dismiss-{}", job.head_bookmark)),
                "Dismiss",
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                app.update(cx, |model, cx| model.dismiss_autoland(&status, cx))
            }))
        };
        let mut buttons = h_flex().flex_shrink_0().gap(px(6.));
        if interrupted {
            if blocker.is_none() {
                let (head, options) = (job.head_bookmark.clone(), job.options);
                buttons = buttons.child(
                    btn(
                        SharedString::from(format!("job-resume-{}", job.head_bookmark)),
                        if job.options.bypass {
                            "Resume with bypass"
                        } else {
                            "Resume watching"
                        },
                    )
                    .disabled(self.resuming || job_active)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.resume_autoland(head.clone(), options, cx)
                    })),
                );
            }
            buttons = buttons.child(dismiss(cx));
        } else if !terminal {
            let app = self.app.clone();
            buttons = buttons.child(
                btn("job-stop", if stopping { "Stopping…" } else { "Stop" })
                    .disabled(stopping)
                    .on_click(move |_, _, cx| app.update(cx, |model, cx| model.stop_autoland(cx))),
            );
        } else {
            buttons = buttons.child(dismiss(cx));
        }
        let line = |text: String, color: Hsla| {
            div()
                .pl(px(15.))
                .text_size(px(TEXT_S))
                .line_height(gpui::relative(1.5))
                .text_color(color)
                .child(text)
        };
        let mut told = story(status);
        if interrupted {
            told.push_str(&format!(
                " Last update {}.",
                crate::shell::topbar::checked_age_label(
                    status.record.saved_at_ms,
                    crate::store::now_ms()
                )
            ));
        }
        let mut band = v_flex()
            .flex_shrink_0()
            .pt(px(8.))
            .px(px(16.))
            .pb(px(12.))
            .border_b_1()
            .border_color(t.border2)
            .child(
                h_flex()
                    .gap(px(8.))
                    .min_h(px(28.))
                    // The job at work keeps a slow heartbeat.
                    .child(if status.live && !terminal {
                        crate::ui::motion::pulse_dot(color, 7.).into_any_element()
                    } else {
                        dot(color, 7.).into_any_element()
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(TEXT_M))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.text1)
                            .child(job_title(status)),
                    )
                    .child(
                        ref_label(None, job.head_bookmark.clone(), cx)
                            .flex_shrink_1()
                            .min_w_0(),
                    )
                    .child(div().flex_1())
                    .child(buttons),
            )
            .child(line(told, t.text2));
        if job.options.bypass {
            band = band.child(line(autoland_options_story(&job.options), t.warn));
        }
        if let Some(blocker) = blocker {
            band = band.child(line(blocker, t.danger));
        }
        if let Some(error) = &self.resume_error {
            band = band.child(line(error.clone(), t.danger));
        }
        if let Some(outcome) = &job.last_outcome {
            let remote = self.job_remote(cx);
            let steps: Vec<_> = outcome
                .steps
                .iter()
                .map(|step| {
                    (
                        step.status,
                        land_action_text(&step.action, &remote),
                        step.detail.clone(),
                    )
                })
                .collect();
            let count = steps.len();
            band = band.child(div().pl(px(15.)).mt(px(6.)).child(self.disclosure(
                &format!("job-round-{}", job.head_bookmark),
                "Last round",
                Some(format!("{count} step{}", if count == 1 { "" } else { "s" })),
                |cx| self.result_steps(steps, cx),
                cx,
            )));
        }
        band.into_any_element()
    }
}

impl Render for PublishView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = theme(cx);
        let (phase, has_repo, forge_error, prs_error, truncated, jobs) = {
            let model = self.app.read(cx);
            let forge = &model.forge;
            let jobs: Vec<AutoLandStatus> =
                visible_jobs(&model.autoland.jobs, model.repo_path().as_deref())
                    .into_iter()
                    .cloned()
                    .collect();
            (
                forge.phase,
                forge
                    .status
                    .as_ref()
                    .is_some_and(|status| status.repo.is_some()),
                forge.error.clone(),
                forge.prs_error.clone(),
                forge.prs.as_ref().is_some_and(|prs| prs.report.truncated),
                jobs,
            )
        };
        let mut root = div()
            .id("publish")
            .key_context("Publish")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|this, _: &FindStack, window, cx| {
                if this.page == Page::Stacks {
                    this.query.update(cx, |input, cx| input.focus(window, cx));
                }
            }))
            .child(self.render_page_bar(window, cx));
        let loading = matches!(phase, ForgePhase::Idle | ForgePhase::Loading);
        let stacks_notice = |this: &Self, error: String, cx: &mut Context<Self>| {
            this.notice(
                "notice-stacks",
                error,
                true,
                Some(("Retry", Box::new(|this, _, cx| this.retry_stacks(cx)))),
                cx,
            )
        };
        let Some(snapshot) = self.snapshot.clone() else {
            // Nothing read yet: what is happening, or why it failed and the
            // way to try again.
            return match self.stacks_error.clone() {
                Some(error) => root.child(stacks_notice(self, error, cx)),
                None => root.child(
                    div()
                        .p(px(48.))
                        .text_color(t.text2)
                        .child("Reading your stacks…"),
                ),
            };
        };
        if !has_repo && !loading {
            return root.child(empty_state(
                IconName::Publish,
                "Add a GitHub remote",
                "Connect this repository to GitHub to publish and land your stacks.",
                Some("jj git remote add origin <url>".into()),
                cx,
            ));
        }
        if let (Some(error), false) = (
            self.action_error.clone().or(forge_error),
            self.connection.is_open(),
        ) {
            root = root.child(self.notice(
                "notice-connection",
                error,
                true,
                Some((
                    "Connection settings",
                    Box::new(|this, window, cx| {
                        this.connection.open(window, cx);
                        cx.notify();
                    }),
                )),
                cx,
            ));
        }
        if let Some(error) = self.stacks_error.clone() {
            root = root.child(stacks_notice(self, error, cx));
        }
        if let Some(error) = prs_error {
            root = root.child(self.notice(
                "notice-prs",
                error,
                true,
                Some(("Retry", Box::new(|this, _, cx| this.refresh_selected(cx)))),
                cx,
            ));
        }
        if truncated {
            root = root.child(
                self.notice(
                    "notice-truncated",
                    "Showing the 100 most recently updated pull requests. Older stacks may be \
                 incomplete."
                        .to_owned(),
                    false,
                    None,
                    cx,
                ),
            );
        }
        for job in &jobs {
            root = root.child(self.render_job(job, &snapshot, cx));
        }
        root.child(match self.page {
            Page::Stacks => self.render_stacks_page(&snapshot, &jobs, window, cx),
            Page::Review => self.render_review_page(&snapshot, window, cx),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    use gpui::{Entity, TestAppContext, WindowHandle};
    use jiji_core::snapshot::SnapshotView;
    use jiji_forge::PrState;
    use jiji_forge::{
        AutoLandPhase, AutoLandRecord, AutoLandState, ChecksRollup, LandAction, LandSegmentStatus,
        MergeMethod, ReviewDecision, StackLink, SubmitAction, SubmitHead,
    };

    use super::{Mode, Page, PublishView};
    use crate::app::{AppModel, Section};
    use crate::shell::Shell;
    use crate::testing::forge::Fixture;

    /// Publish on screen over the mock repository, with `fixture` as its
    /// GitHub, settled: connected, the stacks read, the current stack's
    /// plan in. `pushed` publishes the mock's stack first, so its branches
    /// are what GitHub holds.
    struct Bench {
        dir: PathBuf,
        model: Entity<AppModel>,
        publish: Entity<PublishView>,
        _window: WindowHandle<Shell>,
    }

    fn bench(name: &str, fixture: &Fixture, pushed: bool, cx: &mut TestAppContext) -> Bench {
        let dir = crate::testing::scratch(name);
        bench_in(dir, fixture, pushed, cx)
    }

    fn bench_in(dir: PathBuf, fixture: &Fixture, pushed: bool, cx: &mut TestAppContext) -> Bench {
        let (model, repo) = crate::testing::install_mock_over(&dir, fixture.boxed(), cx);
        if pushed {
            model.read_with(cx, |model, _| {
                let names = ["diff-model".to_owned(), "diff-virtualization".to_owned()];
                model
                    .host
                    .backend()
                    .push_bookmarks(Path::new(&repo), &names, Some("origin"))
                    .unwrap();
            });
        }
        let window = cx.add_window(Shell::new);
        model.update(cx, |model, cx| {
            model.open_paths(vec![PathBuf::from(&repo)], cx)
        });
        cx.run_until_parked();
        model.update(cx, |model, cx| model.go_to_section(Section::Publish, cx));
        cx.run_until_parked();
        let publish = window
            .update(cx, |shell, _, _| shell.section::<PublishView>())
            .unwrap()
            .expect("Publish is on screen");
        Bench {
            dir,
            model,
            publish,
            _window: window,
        }
    }

    impl Bench {
        fn land(&self, cx: &TestAppContext) -> jiji_forge::LandPlan {
            self.publish.read_with(cx, |view, _| {
                assert_eq!(view.land_error, None);
                view.land.clone().expect("a land plan is on the card")
            })
        }

        fn select(&self, key: &str, cx: &mut TestAppContext) {
            self.publish
                .update(cx, |view, cx| view.select_stack(key, cx));
            cx.run_until_parked();
        }
    }

    fn merges(plan: &jiji_forge::LandPlan) -> Vec<(u64, MergeMethod)> {
        plan.actions
            .iter()
            .filter_map(|action| match action {
                LandAction::MergePr { number, method, .. } => Some((*number, *method)),
                _ => None,
            })
            .collect()
    }

    fn blocking(plan: &jiji_forge::LandPlan) -> Vec<String> {
        plan.blockers
            .iter()
            .map(|blocker| blocker.message.clone())
            .collect()
    }

    /// Run the app until `done` holds. An auto-land job works on a thread
    /// of its own, so what it publishes arrives from outside the test's
    /// executor: the test lets it, as GPUI does for real threads.
    fn until(
        cx: &mut TestAppContext,
        what: &str,
        mut done: impl FnMut(&mut TestAppContext) -> bool,
    ) {
        cx.executor().allow_parking();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[gpui::test]
    fn the_current_stack_opens_on_its_land_plan_and_local_work_on_its_publish_plan(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-open", &fixture, true, cx);
        // The working copy's stack is chosen, its two pull requests in
        // landing order, and the plan lands the bottom one first.
        bench.publish.read_with(cx, |view, cx| {
            assert!(view.connected(cx));
            assert_eq!(view.selected.as_deref(), Some("diff-virtualization"));
            assert!(view.mode == Mode::Land && view.page == Page::Stacks);
            let stack = view.selected_stack().unwrap();
            assert!(stack.is_active);
            let prs: Vec<u64> = stack
                .segments
                .iter()
                .filter_map(|segment| segment.pr.as_ref().map(|pr| pr.number))
                .collect();
            assert_eq!(prs, [41, 42]);
            assert_eq!(view.land_for.as_deref(), Some("diff-virtualization"));
        });
        let plan = bench.land(cx);
        assert_eq!(merges(&plan), [(41, MergeMethod::Squash)]);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // Planning only reads.
        assert_eq!(fixture.take_asked(), Vec::<String>::new());

        // Local work answers with its publish plan: a branch to push and a
        // pull request to open.
        bench.select("toznkvpq", cx);
        bench.publish.read_with(cx, |view, _| {
            assert!(view.mode == Mode::Publish && view.land.is_none());
            let plan = view.plan.as_ref().expect("a publish plan is on the card");
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            assert!(plan
                .actions
                .iter()
                .any(|action| matches!(action, SubmitAction::CreatePr { .. })));
        });
        assert_eq!(fixture.take_asked(), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn the_pull_requests_page_leaves_the_stack_and_its_plan_where_they_were(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-pages", &fixture, true, cx);
        let before = bench
            .publish
            .read_with(cx, |view, _| (view.selected.clone(), view.land_seq));
        bench.publish.update(cx, |view, cx| {
            view.page = Page::Review;
            cx.notify();
        });
        cx.run_until_parked();
        // Every open pull request is a row there.
        bench.model.read_with(cx, |model, _| {
            let rows =
                crate::logic::publish::review_rows(model.forge.prs.as_deref(), Some("jiji-mock"));
            assert_eq!(rows.len(), 2);
        });
        bench.publish.update(cx, |view, cx| {
            view.page = Page::Stacks;
            cx.notify();
        });
        cx.run_until_parked();
        // Back on the stacks: the same one chosen, its plan not asked again.
        bench.publish.read_with(cx, |view, _| {
            assert_eq!((view.selected.clone(), view.land_seq), before);
            assert!(view.land.is_some());
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn a_stack_that_differs_from_github_waits_to_be_published(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-unpushed", &fixture, false, cx);
        let plan = bench.land(cx);
        assert!(plan.actions.is_empty());
        assert_eq!(
            blocking(&plan),
            [
                "\u{201c}diff-model\u{201d} and its GitHub branch differ — publish the stack \
              first, so what merges is what you see here"
            ]
        );
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn the_merge_method_bypass_and_drafts_shape_the_land_plan(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-options", &fixture, true, cx);
        let replan = |method: Option<MergeMethod>, bypass: bool, cx: &mut TestAppContext| {
            bench.publish.update(cx, |view, cx| {
                view.land_method = method;
                view.land_bypass = bypass;
                view.replan_land(cx);
            });
            cx.run_until_parked();
            bench.land(cx)
        };

        // The method asked for is the one the merge uses, and one the
        // repository refuses is a blocker, not a quiet substitute.
        let plan = replan(Some(MergeMethod::Rebase), false, cx);
        assert_eq!(merges(&plan), [(41, MergeMethod::Rebase)]);
        assert_eq!(plan.options.method, Some(MergeMethod::Rebase));
        fixture.github().rebase = false;
        let plan = replan(Some(MergeMethod::Rebase), false, cx);
        assert!(merges(&plan).is_empty());
        assert_eq!(
            blocking(&plan),
            ["jiji-mock/workbench does not allow rebase merges — pick another method"]
        );
        let plan = replan(None, false, cx);
        assert_eq!(merges(&plan), [(41, MergeMethod::Squash)]);

        // Failing required checks block; bypass merges past them, and says
        // so.
        fixture.pull(41, |pull| pull.summary.checks = ChecksRollup::Failing);
        let plan = replan(None, false, cx);
        assert_eq!(blocking(&plan), ["#41's checks are failing"]);
        let plan = replan(None, true, cx);
        assert_eq!(merges(&plan), [(41, MergeMethod::Squash)]);
        assert!(plan.options.bypass);
        assert!(
            plan.warnings
                .iter()
                .any(|warning| warning.starts_with("Bypassing failing checks on #41")),
            "{:?}",
            plan.warnings
        );

        // A draft is marked ready as its own step, just before it merges.
        fixture.pull(41, |pull| {
            pull.summary.checks = ChecksRollup::Passing;
            pull.summary.is_draft = true;
        });
        let plan = replan(None, false, cx);
        assert!(matches!(
            plan.actions[..2],
            [
                LandAction::MarkReadyPr { number: 41, .. },
                LandAction::MergePr { number: 41, .. }
            ]
        ));
        assert_eq!(fixture.take_asked(), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn a_plan_that_answers_late_never_lands_on_the_stack_chosen_since(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-late", &fixture, true, cx);
        // The current stack's plan is asked for again, and before GitHub
        // answers, another stack is chosen.
        bench.publish.update(cx, |view, cx| {
            view.load_land_plan("diff-virtualization".into(), cx);
            assert!(view.land_loading);
            view.select_stack("toznkvpq", cx);
        });
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.selected.as_deref(), Some("toznkvpq"));
            assert!(view.mode == Mode::Publish);
            assert!(view.land.is_none() && !view.land_loading);
            assert!(view.plan.is_some());
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn failed_ci_is_sent_back_to_work_and_the_badges_follow(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        fixture.pull(42, |pull| {
            pull.summary.checks = ChecksRollup::Failing;
            pull.failed_runs = vec!["ci / test".into()];
        });
        let bench = bench("publish-rerun", &fixture, true, cx);
        bench.publish.update(cx, |view, cx| {
            view.page = Page::Review;
            view.rerun_ci(42, cx);
        });
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.rerun_busy, None);
            assert_eq!(
                view.rerun_note.as_deref(),
                Some("#42: Re-running failed jobs in ci / test.")
            );
        });
        assert_eq!(fixture.take_asked(), ["rerun c5e208d4"]);
        // The refresh that follows reads the checks running again.
        bench.model.read_with(cx, |model, _| {
            let prs = model.forge.prs.as_ref().unwrap();
            assert_eq!(
                prs.by_branch["diff-virtualization"].checks,
                ChecksRollup::Pending
            );
        });

        // Nothing failed this time: the note says so plainly.
        bench.publish.update(cx, |view, cx| view.rerun_ci(42, cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            let note = view.rerun_note.clone().unwrap();
            assert!(note.starts_with("#42: "), "{note}");
            assert_ne!(note, "#42: Re-running failed jobs in ci / test.");
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn a_pull_request_is_fetched_for_review_by_its_row_or_its_number(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-review", &fixture, false, cx);
        let window = bench._window;
        // By number: GitHub is asked for that one pull request, and the
        // panel opens on it with a bookmark name to take.
        window
            .update(cx, |_, window, cx| {
                bench.publish.update(cx, |view, cx| {
                    view.page = Page::Review;
                    view.lookup.update(cx, |input, cx| input.set_text("41", cx));
                    view.lookup_pr(window, cx);
                })
            })
            .unwrap();
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, cx| {
            assert_eq!(view.review_for.as_ref().map(|pr| pr.number), Some(41));
            assert_eq!(view.review_name.read(cx).text(), "pr/41");
        });
        bench
            .publish
            .update(cx, |view, cx| view.fetch_for_review(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.review_error, None);
            assert!(view.review_for.is_none());
            let (bookmark, change) = view.review_done.clone().expect("the fetch landed");
            assert_eq!(bookmark, "pr/41");
            assert!(change.is_some());
        });
        // The bookmark is the repository's now: the same name is refused.
        let snapshot = bench
            .model
            .read_with(cx, |model, _| model.snapshot().cloned().unwrap());
        assert!(snapshot.bookmarks.iter().any(|b| b.name == "pr/41"));
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn publishing_opens_the_pull_requests_the_plan_named_and_keeps_the_result(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let bench = bench("publish-submit", &fixture, false, cx);
        // A stack with no bookmark: the plan names one, pushes it and opens
        // its pull request.
        bench.select("toznkvpq", cx);
        bench.publish.update(cx, |view, cx| view.publish(cx));
        cx.run_until_parked();
        let branch = "feat-palette-matcher-scores-revset-atoms";
        assert_eq!(
            fixture.take_asked(),
            [format!("create #41 {branch} → main")]
        );
        // The badges follow what GitHub now holds, and the stack, known by
        // its new bookmark now, stays chosen with what was done on it.
        bench.model.read_with(cx, |model, _| {
            let prs = model.forge.prs.as_ref().unwrap();
            assert_eq!(prs.by_branch[branch].number, 41);
        });
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.plan_error, None);
            assert_eq!(view.selected.as_deref(), Some(branch));
            let outcome = view.outcome.as_ref().expect("the publish's steps stay");
            assert!(!outcome.failed, "{:?}", outcome.steps);
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn landing_merges_on_github_then_tidies_the_stack_here(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-land", &fixture, true, cx);
        bench.publish.update(cx, |view, cx| view.land_stack(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.land_error, None);
            let outcome = view.land_outcome.as_ref().expect("the land ran");
            assert!(!outcome.failed, "{:?}", outcome.steps);
        });
        // #41 merged, and #42, rebased onto trunk here, targets it now.
        // It is ready already, so it is left for the next Land to merge
        // rather than handed to auto-merge.
        assert_eq!(
            fixture.take_asked(),
            ["merge #41 by squash", "retarget #42 → main"]
        );
        // The landed segment is gone from here and from the remote.
        let snapshot = bench
            .model
            .read_with(cx, |model, _| model.snapshot().cloned().unwrap());
        assert!(!snapshot.bookmarks.iter().any(|b| b.name == "diff-model"));
        assert!(!snapshot.nodes.iter().any(|node| node.id == "nzpwlxvr"));
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn a_pull_request_github_closed_without_landing_it_is_not_treated_as_landed(
        cx: &mut TestAppContext,
    ) {
        // What GitHub reports after a reordered push, as seen on the real
        // thing: #41 closed as merged into the branch above it, "as" its
        // own head commit. Nothing reached trunk.
        let fixture = Fixture::mock_stack();
        fixture.pull(41, |pull| {
            pull.summary.base_branch = "diff-virtualization".into();
            pull.merged_as("8b19f3c2");
        });
        fixture.pull(42, |pull| pull.summary.base_branch = "main".into());
        let bench = bench("publish-false-merge", &fixture, true, cx);
        let plan = bench.land(cx);
        assert!(plan.actions.is_empty(), "{:?}", plan.actions);
        let blockers = blocking(&plan);
        assert_eq!(blockers.len(), 1, "{blockers:?}");
        assert!(
            blockers[0].contains("#41 (diff-model) is closed as merged on GitHub")
                && blockers[0].contains(
                    "merged into \u{201c}diff-virtualization\u{201d}, not \u{201c}main\u{201d}"
                ),
            "{blockers:?}"
        );
        // The ledger does not show it as merged.
        assert!(plan
            .segments
            .iter()
            .all(|segment| !matches!(segment.status, LandSegmentStatus::Merged { .. })));
        // Pressing Land does nothing: no request, and the change and its
        // bookmark are still here.
        bench.publish.update(cx, |view, cx| view.land_stack(cx));
        cx.run_until_parked();
        assert_eq!(fixture.take_asked(), Vec::<String>::new());
        let snapshot = bench
            .model
            .read_with(cx, |model, _| model.snapshot().cloned().unwrap());
        assert!(snapshot.bookmarks.iter().any(|b| b.name == "diff-model"));
        assert!(snapshot.nodes.iter().any(|node| node.id == "nzpwlxvr"));
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn a_github_stack_lands_in_one_request_and_says_so(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        assert_eq!(fixture.link_stack(&[41, 42]), 90);
        fixture.take_asked();
        let bench = bench("publish-land-stack", &fixture, true, cx);
        // The stack is known as GitHub's, and the round takes both.
        bench.publish.read_with(cx, |view, _| {
            let linked = view.selected_stack().unwrap().github_stack.unwrap();
            assert_eq!((linked.number, linked.linked), (90, 2));
        });
        let plan = bench.land(cx);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(merges(&plan), [(42, MergeMethod::Squash)]);
        assert!(plan.actions.iter().any(|action| matches!(
            action,
            LandAction::MergePr { number: 42, with, stack: Some(90), .. }
                if with.len() == 1 && with[0].number == 41
        )));
        assert!(!plan.actions.iter().any(|action| matches!(
            action,
            LandAction::RetargetPr { .. } | LandAction::EnableAutoMerge { .. }
        )));
        assert_eq!(
            crate::logic::publish::manual_land_label(&plan),
            "Land ready PRs"
        );

        bench.publish.update(cx, |view, cx| view.land_stack(cx));
        cx.run_until_parked();
        // The whole stack landed, so there is no stack left on the page
        // to hold a result card; what happened is read off GitHub and the
        // repository.
        bench
            .publish
            .read_with(cx, |view, _| assert_eq!(view.land_error, None));
        // One request through the stacked endpoint; the plain one, which
        // GitHub refuses for a stack, was never asked.
        assert_eq!(
            fixture.take_asked(),
            ["merge #41, #42 together by squash (stack 90)"]
        );
        let snapshot = bench
            .model
            .read_with(cx, |model, _| model.snapshot().cloned().unwrap());
        assert!(!snapshot
            .bookmarks
            .iter()
            .any(|b| b.name == "diff-model" || b.name == "diff-virtualization"));
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn publish_links_a_github_stack_only_when_asked(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-link-stack", &fixture, true, cx);
        let head = SubmitHead::Bookmark {
            name: "diff-virtualization".into(),
        };
        let link_steps = |view: &PublishView| -> Vec<SubmitAction> {
            view.plan
                .as_ref()
                .expect("a publish plan is on the card")
                .actions
                .iter()
                .filter(|action| {
                    matches!(
                        action,
                        SubmitAction::LinkStack { .. } | SubmitAction::Unstack { .. }
                    )
                })
                .cloned()
                .collect()
        };
        // Left alone, the chain stays a chain.
        bench
            .publish
            .update(cx, |view, cx| view.load_plan(head.clone(), cx));
        cx.run_until_parked();
        bench
            .publish
            .read_with(cx, |view, _| assert_eq!(link_steps(view), []));

        // Asked, the plan ends by linking both, bottom first.
        bench.publish.update(cx, |view, cx| {
            view.stack_link = StackLink::Link;
            view.load_plan(head.clone(), cx)
        });
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(
                link_steps(view),
                [SubmitAction::LinkStack {
                    bookmarks: vec!["diff-model".into(), "diff-virtualization".into()],
                    onto: None,
                    above: vec![],
                }]
            );
        });
        bench.publish.update(cx, |view, cx| view.publish(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.plan_error, None);
            let outcome = view.outcome.as_ref().expect("the publish ran");
            assert!(!outcome.failed, "{:?}", outcome.steps);
        });
        assert!(fixture
            .take_asked()
            .contains(&"stack 90 = [41, 42]".to_owned()));
        // The stack reads as GitHub's from here on, with nothing left to
        // ask for.
        bench.publish.update(cx, |view, cx| {
            view.stack_link = StackLink::Follow;
            view.load_plan(head.clone(), cx)
        });
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(link_steps(view), []);
            let linked = view.selected_stack().unwrap().github_stack.unwrap();
            assert_eq!((linked.number, linked.linked), (90, 2));
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    /// The setting in Settings: a publish that opens pull requests for a
    /// stack links them without being asked. It is kept where the `jiji`
    /// command reads it, and the card's own choice still wins.
    #[gpui::test]
    fn the_setting_links_a_new_stack_and_the_card_can_still_say_no(cx: &mut TestAppContext) {
        // Nothing is published yet: the plan opens both pull requests.
        let fixture = Fixture::new();
        let bench = bench("publish-link-setting", &fixture, false, cx);
        let head = SubmitHead::Bookmark {
            name: "diff-virtualization".into(),
        };
        let links = |view: &PublishView| -> usize {
            view.plan
                .as_ref()
                .expect("a publish plan is on the card")
                .actions
                .iter()
                .filter(|action| matches!(action, SubmitAction::LinkStack { .. }))
                .count()
        };
        let replan = |cx: &mut TestAppContext| {
            bench
                .publish
                .update(cx, |view, cx| view.load_plan(head.clone(), cx));
            cx.run_until_parked();
        };

        // Off, as it is unless turned on: a plain chain.
        replan(cx);
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(links(view), 0);
            assert_eq!(view.plan.as_ref().unwrap().options.stack, StackLink::Follow);
        });
        let data = bench.dir.join("data");
        assert!(!jiji_forge::prefs::link_new_stacks(&data));

        // Turned on in Settings: written to the file the command reads,
        // and the same plan now ends by linking what it opens.
        bench
            .model
            .update(cx, |model, cx| model.set_link_new_stacks(true, cx));
        assert!(bench
            .model
            .read_with(cx, |model, _| model.link_new_stacks()));
        assert!(jiji_forge::prefs::link_new_stacks(&data));
        replan(cx);
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(
                view.plan.as_ref().unwrap().options.stack,
                StackLink::LinkNew
            );
            assert_eq!(links(view), 1);
        });

        // The card says no for this publish, and that holds against the
        // setting.
        bench.publish.update(cx, |view, cx| {
            view.stack_link = StackLink::Unlink;
            view.load_plan(head.clone(), cx)
        });
        cx.run_until_parked();
        bench
            .publish
            .read_with(cx, |view, _| assert_eq!(links(view), 0));

        // Left to the setting again, the publish links the stack.
        bench.publish.update(cx, |view, cx| {
            view.stack_link = StackLink::Follow;
            view.load_plan(head.clone(), cx)
        });
        cx.run_until_parked();
        bench.publish.update(cx, |view, cx| view.publish(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.plan_error, None);
            let outcome = view.outcome.as_ref().expect("the publish ran");
            assert!(!outcome.failed, "{:?}", outcome.steps);
        });
        assert!(fixture
            .take_asked()
            .contains(&"stack 90 = [41, 42]".to_owned()));
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn shipping_moves_trunk_to_the_stack_and_pushes_it(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let bench = bench("publish-ship", &fixture, false, cx);
        bench.select("toznkvpq", cx);
        let head = bench
            .publish
            .read_with(cx, |view, _| {
                view.selected_stack().unwrap().head_change.clone()
            })
            .unwrap();
        bench
            .publish
            .update(cx, |view, cx| view.load_ship_plan(head.clone(), cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert!(view.mode == Mode::Ship);
            assert_eq!(view.ship_error, None);
            let plan = view.ship.as_ref().expect("a ship plan is on the card");
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            assert!(!plan.actions.is_empty());
        });
        bench.publish.update(cx, |view, cx| view.ship_stack(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.ship_error, None);
            let outcome = view.ship_outcome.as_ref().expect("the ship ran");
            assert!(!outcome.failed, "{:?}", outcome.steps);
        });
        let snapshot = bench
            .model
            .read_with(cx, |model, _| model.snapshot().cloned().unwrap());
        let trunk = snapshot.bookmarks.iter().find(|b| b.is_trunk).unwrap();
        assert_eq!(trunk.target, head);
        assert_eq!(trunk.sync, jiji_core::snapshot::SyncState::Synced);
        // Shipping is git's alone: GitHub was asked nothing.
        assert_eq!(fixture.take_asked(), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    /// Shipping a published stack finishes its pull requests: the upper
    /// one is pointed at trunk before the push, and both are merged by it.
    #[gpui::test]
    fn shipping_a_published_stack_merges_every_pull_request_of_it(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-ship-prs", &fixture, true, cx);
        let head = bench
            .publish
            .read_with(cx, |view, _| {
                view.selected_stack().unwrap().head_change.clone()
            })
            .unwrap();
        bench
            .publish
            .update(cx, |view, cx| view.load_ship_plan(head.clone(), cx));
        cx.run_until_parked();
        // The card names each pull request and what happens to it.
        let steps: Vec<String> = bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.ship_error, None);
            let plan = view.ship.as_ref().expect("a ship plan is on the card");
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            plan.actions
                .iter()
                .map(crate::logic::publish::ship_action_text)
                .collect()
        });
        assert!(
            steps.contains(
                &"Retarget #42 (diff-virtualization) onto main — the push then merges it"
                    .to_owned()
            ),
            "{steps:?}"
        );
        assert!(
            steps.contains(&"Check that GitHub marked #41 and #42 merged".to_owned()),
            "{steps:?}"
        );
        bench.publish.update(cx, |view, cx| view.ship_stack(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.ship_error, None);
            let outcome = view.ship_outcome.as_ref().expect("the ship ran");
            assert!(!outcome.failed, "{:?}", outcome.steps);
            assert!(outcome.steps.iter().any(|step| {
                step.detail.as_deref() == Some("GitHub marked #41 and #42 merged")
            }));
        });
        // The one thing asked of GitHub that changes something, and it
        // was asked before trunk moved.
        assert_eq!(fixture.take_asked(), ["retarget #42 → main"]);
        let merged = |number: u64| {
            fixture
                .github()
                .prs
                .iter()
                .find(|pull| pull.summary.number == number)
                .map(|pull| pull.summary.state)
        };
        assert_eq!(merged(41), Some(PrState::Merged));
        assert_eq!(merged(42), Some(PrState::Merged));
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    /// A pull request someone else opened on the stack shows with it and
    /// is no part of it: the stack keeps its two pull requests, and a
    /// landing points theirs at trunk before the branch under it goes.
    /// Ship needs no GitHub to push. With no token its plan knows of no
    /// pull requests and says what GitHub will do all the same. With a
    /// token and a read that fails, there is no plan: a ship that could
    /// not see the pull requests would leave them open without a word.
    /// (Found in review: the failed read was planned around in silence.)
    #[gpui::test]
    fn a_ship_tells_a_failed_read_of_github_and_says_what_it_leaves_without_a_token(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::mock_stack();
        let bench = bench("publish-ship-unread", &fixture, true, cx);
        let head = bench
            .publish
            .read_with(cx, |view, _| {
                view.selected_stack().unwrap().head_change.clone()
            })
            .unwrap();

        fixture.github().refuse_prs = Some("GitHub is having a moment".into());
        bench
            .publish
            .update(cx, |view, cx| view.load_ship_plan(head.clone(), cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert!(view.ship.is_none(), "no plan is made around a failed read");
            let told = view.ship_error.as_deref().unwrap_or_default();
            assert!(told.contains("GitHub is having a moment"), "{told}");
        });

        fixture.github().token = None;
        bench
            .publish
            .update(cx, |view, cx| view.load_ship_plan(head.clone(), cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.ship_error, None);
            let plan = view.ship.as_ref().expect("a plan without pull requests");
            assert!(!plan
                .actions
                .iter()
                .any(|action| matches!(action, jiji_forge::ShipAction::RetargetPr { .. })));
            assert!(
                plan.warnings
                    .iter()
                    .any(|note| note == jiji_forge::SHIPPED_WITHOUT_GITHUB),
                "{:?}",
                plan.warnings
            );
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn someone_elses_pull_request_shows_with_the_stack_and_is_only_retargeted(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::mock_stack();
        {
            let mut theirs = crate::testing::forge::open_pr(
                50,
                "their-fix",
                "diff-model",
                "0ddba11c",
                "A fix on top of the sticky headers",
            );
            theirs.summary.author = Some("kim".into());
            fixture.github().prs.push(theirs);
        }
        let bench = bench("publish-others", &fixture, true, cx);
        bench.publish.read_with(cx, |view, _| {
            let stack = view.selected_stack().expect("the mock stack is selected");
            assert_eq!(stack.pr_count, 2);
            assert_eq!(
                stack
                    .others
                    .iter()
                    .map(|pr| (pr.number, pr.by_author(), pr.base_branch.as_str()))
                    .collect::<Vec<_>>(),
                [(50, "#50 by @kim".to_owned(), "diff-model")]
            );
        });
        // Drawn in the ledger, with the segment it rests on, and not as a
        // third station of the stack.
        {
            let mut visual = gpui::VisualTestContext::from_window(bench._window.into(), cx);
            assert!(
                visual.debug_bounds("seg-other-50").is_some(),
                "their pull request has a row of its own"
            );
            assert!(visual.debug_bounds("seg-other-41").is_none());
        }
        let plan = bench.land(cx);
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        // Only the stack's own bottom pull request merges.
        assert_eq!(merges(&plan), [(41, MergeMethod::Squash)]);
        let steps: Vec<String> = plan
            .actions
            .iter()
            .map(|action| crate::logic::publish::land_action_text(action, &plan.remote))
            .collect();
        assert!(
            steps.contains(
                &"Retarget #50 by @kim (their-fix) onto main: its base diff-model lands and \
                  is removed"
                    .to_owned()
            ),
            "{steps:?}"
        );
        // That is the one step for it: nothing pushes to its branch, and
        // the plan says what it will show.
        assert!(!steps
            .iter()
            .any(|step| step.contains("their-fix") && step.starts_with("Rebase")));
        assert!(
            plan.warnings.iter().any(|note| note.contains(
                "#50 by @kim rests on this stack and is someone else's: Land never merges it and \
                 never pushes to its branch"
            )),
            "{:?}",
            plan.warnings
        );
        bench.publish.update(cx, |view, cx| view.land_stack(cx));
        cx.run_until_parked();
        bench
            .publish
            .read_with(cx, |view, _| assert_eq!(view.land_error, None));
        let asked = fixture.take_asked();
        assert!(
            asked.contains(&"retarget #50 → main".to_owned()),
            "{asked:?}"
        );
        assert!(
            !asked.iter().any(|what| what.contains("merge #50")),
            "{asked:?}"
        );
        // Theirs is open still, and on trunk.
        let github = fixture.github();
        let theirs = github
            .prs
            .iter()
            .find(|pull| pull.summary.number == 50)
            .unwrap();
        assert_eq!(theirs.summary.state, PrState::Open);
        assert_eq!(theirs.summary.base_branch, "main");
        drop(github);
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    /// A teammate's branch of the mock repository, as a pull request on
    /// top of the mock stack: `alice/palette-icons` is on the remote and
    /// is no bookmark here.
    fn alice_on_top(fixture: &Fixture) {
        let mut theirs = crate::testing::forge::open_pr(
            52,
            "alice/palette-icons",
            "diff-virtualization",
            "19b753c4",
            "Icon set for the palette rows",
        );
        theirs.summary.author = Some("alice".into());
        fixture.github().prs.push(theirs);
    }

    /// Their pull request is taken over from its own row. The click asks:
    /// the card says what will happen and nothing runs. Confirmed, their
    /// branch is a bookmark here and their pull request the stack's next
    /// station, marked as taken over, and the plans that follow name it.
    /// Giving back is the same two steps the other way.
    #[gpui::test]
    fn their_pull_request_is_taken_over_from_its_row_and_given_back(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        alice_on_top(&fixture);
        let bench = bench("publish-take-over", &fixture, true, cx);
        bench.model.read_with(cx, |model, _| {
            assert_eq!(
                model.takeable_prs(),
                [(
                    52,
                    "#52 by @alice".to_owned(),
                    "Icon set for the palette rows".to_owned()
                )]
            );
            assert!(model.taken_over_prs().is_empty());
        });
        // The action is on their row, and a click on it asks.
        {
            let mut visual = gpui::VisualTestContext::from_window(bench._window.into(), cx);
            let action = visual
                .debug_bounds("take-over-52")
                .expect("their row offers to take it over");
            assert!(visual.debug_bounds("give-back-52").is_none());
            visual.simulate_click(action.center(), gpui::Modifiers::none());
        }
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.takeover_for, Some(52));
            let plan = view.takeover.as_ref().expect("the plan is on the card");
            assert!(!plan.giving_back);
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            assert_eq!(plan.who(), "#52 by @alice");
            // What it means is on the card, in full.
            assert_eq!(plan.warnings.len(), 4, "{:?}", plan.warnings);
            assert!(plan.warnings[0].starts_with("Nothing is pushed now."));
            // And nothing ran.
            assert!(view.takeover_outcome.is_none());
            assert!(view.snapshot.as_ref().unwrap().taken_over.is_empty());
        });
        assert!(fixture
            .take_asked()
            .iter()
            .all(|what| !what.contains("#52")));

        bench
            .publish
            .update(cx, |view, cx| view.confirm_take_over(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.takeover_error, None);
            let outcome = view.takeover_outcome.as_ref().expect("the take-over ran");
            assert!(!outcome.failed, "{:?}", outcome.steps);
            assert_eq!(
                outcome.steps.last().unwrap().detail.as_deref(),
                Some(
                    "Took over \u{201c}alice/palette-icons\u{201d}: it is a bookmark here now, at \
                     19b753c4, and nothing was pushed"
                )
            );
            // The stack on screen is the one with theirs in it: a third
            // station, marked, and no longer someone else's.
            let stack = view.selected_stack().expect("a stack is selected");
            assert_eq!(stack.key, "alice/palette-icons");
            assert_eq!(
                stack
                    .segments
                    .iter()
                    .map(|s| (
                        s.pr.as_ref().map(|pr| pr.number),
                        s.taken_over.as_ref().map(|t| t.by_author())
                    ))
                    .collect::<Vec<_>>(),
                [
                    (Some(41), None),
                    (Some(42), None),
                    (Some(52), Some("#52 by @alice".to_owned())),
                ]
            );
            assert!(stack.others.is_empty());
        });
        bench.model.read_with(cx, |model, _| {
            assert!(model.takeable_prs().is_empty());
            assert_eq!(
                model.taken_over_prs(),
                [(
                    52,
                    "#52 by @alice".to_owned(),
                    "alice/palette-icons".to_owned()
                )]
            );
        });
        {
            let mut visual = gpui::VisualTestContext::from_window(bench._window.into(), cx);
            assert!(
                visual.debug_bounds("seg-taken-52").is_some(),
                "the mark is drawn"
            );
            assert!(visual.debug_bounds("give-back-52").is_some());
            assert!(visual.debug_bounds("seg-other-52").is_none());
        }
        // Read, the card gives way to the stack's own plan, which names
        // the pull request that was taken over.
        bench
            .publish
            .update(cx, |view, cx| view.dismiss_take_over(cx));
        cx.run_until_parked();
        let plan = bench.land(cx);
        assert!(
            plan.warnings.iter().any(|note| note.starts_with(
                "#52 by @alice was taken over, and lands with this stack like a pull request of \
                 your own"
            )),
            "{:?}",
            plan.warnings
        );
        assert!(!plan
            .warnings
            .iter()
            .any(|note| note.contains("someone else's")));

        // The way back, from the segment.
        {
            let mut visual = gpui::VisualTestContext::from_window(bench._window.into(), cx);
            let back = visual.debug_bounds("give-back-52").unwrap();
            visual.simulate_click(back.center(), gpui::Modifiers::none());
        }
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            let plan = view.takeover.as_ref().expect("the plan is on the card");
            assert!(plan.giving_back);
            assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
            assert_eq!(plan.title, "Icon set for the palette rows");
            assert_eq!(view.snapshot.as_ref().unwrap().taken_over.len(), 1);
        });
        bench
            .publish
            .update(cx, |view, cx| view.confirm_take_over(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.takeover_error, None);
            assert!(!view.takeover_outcome.as_ref().unwrap().failed);
            assert!(view.snapshot.as_ref().unwrap().taken_over.is_empty());
            // Theirs again: on the stack's top, and no station of it.
            let stack = view.selected_stack().expect("a stack is selected");
            assert_eq!(stack.segments.len(), 2);
            assert_eq!(
                stack.others.iter().map(|pr| pr.number).collect::<Vec<_>>(),
                [52]
            );
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    /// Their pull request is also a stack of its own in the browser, one
    /// that is on GitHub only. Asked from there, the take-over ends on
    /// the stack it has become a part of.
    #[gpui::test]
    fn a_stack_that_is_on_github_only_is_taken_over_from_its_own_card(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        alice_on_top(&fixture);
        let bench = bench("publish-take-over-remote", &fixture, true, cx);
        bench.select("remote:alice/palette-icons", cx);
        bench.publish.read_with(cx, |view, _| {
            let stack = view.selected_stack().unwrap();
            assert!(stack.submit_head.is_none(), "on GitHub only");
            assert_eq!(stack.segments[0].pr.as_ref().unwrap().number, 52);
        });
        // What the card's "Take over #52…" does.
        bench
            .publish
            .update(cx, |view, cx| view.ask_take_over(52, cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.selected.as_deref(), Some("remote:alice/palette-icons"));
            assert!(view
                .takeover
                .as_ref()
                .is_some_and(|plan| plan.blockers.is_empty()));
        });
        bench
            .publish
            .update(cx, |view, cx| view.confirm_take_over(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.takeover_error, None);
            let stack = view.selected_stack().expect("a stack is selected");
            assert_eq!(stack.key, "alice/palette-icons");
            assert!(stack.submit_head.is_some(), "a stack of the user's now");
            assert_eq!(stack.segments.len(), 3);
            assert!(!view
                .stacks
                .iter()
                .any(|s| s.key == "remote:alice/palette-icons"));
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    /// A pull request from a fork has the same action, and the card says
    /// why it cannot be taken over. There is nothing to confirm.
    #[gpui::test]
    fn a_fork_s_pull_request_is_refused_on_the_card_with_the_reason(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        alice_on_top(&fixture);
        fixture.pull(52, |pull| pull.summary.head_owner = Some("alice".into()));
        let bench = bench("publish-take-over-fork", &fixture, true, cx);
        // Asked from the palette's row, which opens the same card.
        bench.model.update(cx, |model, cx| {
            model.send_intent(crate::app::UiIntent::TakeOver { number: 52 }, cx)
        });
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert_eq!(view.takeover_for, Some(52));
            let plan = view.takeover.as_ref().expect("the plan is on the card");
            assert_eq!(plan.blockers.len(), 1, "{:?}", plan.blockers);
            assert!(
                plan.blockers[0].starts_with(
                    "#52 by @alice is from a fork (alice/workbench): its branch \
                     \u{201c}alice/palette-icons\u{201d} is not on origin"
                ),
                "{:?}",
                plan.blockers
            );
        });
        bench
            .publish
            .update(cx, |view, cx| view.confirm_take_over(cx));
        cx.run_until_parked();
        bench.publish.read_with(cx, |view, _| {
            assert!(view.takeover_outcome.is_none());
            assert!(view.snapshot.as_ref().unwrap().taken_over.is_empty());
        });
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn auto_land_starts_stops_resumes_and_is_dismissed(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        // Checks still running: the job hands #41 to GitHub's auto-merge
        // and watches.
        fixture.pull(41, |pull| pull.summary.checks = ChecksRollup::Pending);
        let bench = bench("publish-autoland", &fixture, true, cx);
        let records = bench.dir.join("data").join("autoland.json");
        let jobs = |bench: &Bench, cx: &mut TestAppContext| {
            bench
                .model
                .read_with(cx, |model, _| model.autoland.jobs.clone())
        };
        bench.publish.update(cx, |view, cx| view.queue_autoland(cx));
        // Queued, the record already reads as watching: wait for the round
        // that handed #41 over.
        until(cx, "the job's first round", |cx| {
            jobs(&bench, cx).iter().any(|job| {
                job.live
                    && job.record.state.rounds == 1
                    && matches!(job.record.state.phase, AutoLandPhase::Waiting { .. })
            })
        });
        assert_eq!(fixture.take_asked(), ["auto-merge #41 by squash"]);
        bench.publish.read_with(cx, |view, cx| {
            assert!(view.job_active(cx));
            assert_eq!(view.queue_error, None);
        });

        bench.model.update(cx, |model, cx| model.stop_autoland(cx));
        until(cx, "the job to stop", |cx| {
            jobs(&bench, cx)
                .iter()
                .any(|job| !job.live && job.record.state.phase == AutoLandPhase::Stopped)
        });
        let stopped = jobs(&bench, cx);
        bench
            .model
            .update(cx, |model, cx| model.dismiss_autoland(&stopped[0], cx));
        cx.run_until_parked();
        assert!(jobs(&bench, cx).is_empty());
        assert!(jiji_forge::load_autoland_records(&records).is_empty());
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn an_interrupted_job_resumes_with_its_saved_rules(cx: &mut TestAppContext) {
        let fixture = Fixture::mock_stack();
        fixture.pull(41, |pull| {
            pull.summary.review = ReviewDecision::ReviewRequired
        });
        // The last session was watching this stack, with bypass on, when
        // it ended.
        let dir = crate::testing::scratch("publish-resume");
        let repo = dir.join("repo").to_string_lossy().into_owned();
        let mut state = AutoLandState::queued("diff-virtualization");
        state.phase = AutoLandPhase::Waiting {
            attention: false,
            reasons: vec!["#41 still needs an approving review".into()],
        };
        state.options.bypass = true;
        std::fs::create_dir_all(dir.join("data")).unwrap();
        let records = dir.join("data").join("autoland.json");
        jiji_forge::save_autoland_records(
            &records,
            &[AutoLandRecord {
                repo_path: repo,
                state,
                saved_at_ms: 0,
            }],
        )
        .unwrap();
        let bench = bench_in(dir, &fixture, true, cx);
        let jobs = |bench: &Bench, cx: &mut TestAppContext| {
            bench
                .model
                .read_with(cx, |model, _| model.autoland.jobs.clone())
        };
        let interrupted = jobs(&bench, cx);
        assert_eq!(interrupted.len(), 1);
        assert!(crate::logic::autoland::is_interrupted(&interrupted[0]));

        // Resume sends the rules the card shows; bypass merges at once.
        let options = interrupted[0].record.state.options;
        bench.publish.update(cx, |view, cx| {
            view.resume_autoland("diff-virtualization".into(), options, cx)
        });
        until(cx, "the resumed job to merge", |_| {
            fixture
                .github()
                .asked
                .iter()
                .any(|asked| asked == "merge #41 by squash")
        });
        bench.model.update(cx, |model, cx| model.stop_autoland(cx));
        until(cx, "the job to come to rest", |cx| {
            jobs(&bench, cx).iter().all(|job| !job.live)
        });
        bench
            .publish
            .read_with(cx, |view, _| assert_eq!(view.resume_error, None));
        let _ = std::fs::remove_dir_all(bench.dir);
    }

    #[gpui::test]
    fn retry_reads_the_stacks_again_when_nothing_else_moved(cx: &mut TestAppContext) {
        let dir = crate::testing::scratch("publish-retry");
        let (model, repo) = crate::testing::install_mock(&dir, cx);
        let window = cx.add_window(Shell::new);
        model.update(cx, |model, cx| {
            model.open_paths(vec![PathBuf::from(&repo)], cx)
        });
        cx.run_until_parked();
        // A lens the graph narrows: Publish reads the complete stacks from
        // the repository itself, the read that fails below.
        model.update(cx, |model, cx| {
            model
                .set_snapshot_view(SnapshotView::Mine, false, cx)
                .detach()
        });
        cx.run_until_parked();

        let (jj, away) = (Path::new(&repo).join(".jj"), dir.join("jj-away"));
        std::fs::rename(&jj, &away).unwrap();
        model.update(cx, |model, cx| model.go_to_section(Section::Publish, cx));
        cx.run_until_parked();
        let publish = window
            .update(cx, |shell, _, _| shell.section::<PublishView>())
            .unwrap()
            .expect("Publish is on screen");
        let (error, stacks) =
            publish.read_with(cx, |view, _| (view.stacks_error.clone(), view.stacks.len()));
        assert!(error.is_some() && stacks == 0);

        // The local problem goes away; the snapshot and the (absent) PR
        // state stay exactly as they were.
        std::fs::rename(&away, &jj).unwrap();
        publish.update(cx, |view, cx| view.retry_stacks(cx));
        cx.run_until_parked();
        let (error, stacks) =
            publish.read_with(cx, |view, _| (view.stacks_error.clone(), view.stacks.len()));
        assert_eq!(error, None);
        assert!(stacks > 0);
        model.read_with(cx, |model, _| assert_eq!(model.section(), Section::Publish));
        let _ = std::fs::remove_dir_all(dir);
    }
}

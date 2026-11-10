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
    div, prelude::*, px, AnyElement, Context, Corner, Entity, FocusHandle, Focusable, FontWeight,
    Hsla, ScrollHandle, SharedString, Subscription, Window,
};
use jiji_core::snapshot::RepoSnapshot;
use jiji_forge::{
    AutoLandStatus, LandOptions, LandOutcome, LandPlan, MergeMethod, PrSummary, PublishStack,
    RepoPrState, ShipOutcome, ShipPlan, SubmitHead, SubmitOutcome, SubmitPlan, SubmitStepStatus,
    TokenSource,
};

use crate::app::{app, AppEvent, AppModel, ForgePhase, UiIntent};
use crate::logic::autoland::{is_interrupted, live_job, resume_blocker, story, visible_jobs};
use crate::logic::inspect::Tone;
use crate::logic::publish::{
    autoland_options_story, job_title, job_tone, land_action_text, review_rows,
};
use crate::shell::Dismiss;
use crate::theme::{theme, Theme, TEXT_L, TEXT_M, TEXT_S, TEXT_XS, TOOLBAR_H};
use crate::ui::{
    btn, dot, empty_state, floating, h_flex, icon, icon_btn, input::field, popover, ref_label,
    segmented, v_flex, IconName, InputEvent, PopoverState, SwallowClicks, TextInput,
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

pub(super) fn tone_color(t: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Ok => t.ok,
        Tone::Warn => t.warn,
        Tone::Danger => t.danger,
        Tone::Muted => t.text3,
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
                .masked()
        });
        let query = cx.new(|cx| TextInput::new(window, cx).placeholder("Find a stack…"));
        let review_name = cx.new(|cx| TextInput::new(window, cx).placeholder("Local bookmark"));
        let lookup = cx.new(|cx| TextInput::new(window, cx).placeholder("PR number"));
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
            connection: PopoverState::default(),
            page: Page::Stacks,
            selected: None,
            query,
            show_local: false,
            mode: Mode::Land,
            more: PopoverState::default(),
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
        self.publishing || self.landing || self.shipping || self.queueing
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
                        this.snapshot = Some(snapshot);
                        this.stacks = stacks;
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
                    && self.ship_outcome.is_none();
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
                matches!(intent, UiIntent::Land { .. } | UiIntent::Ship { .. })
            })
        });
        match intent {
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
            .child(div().flex_1().min_w_0().child(text));
        if let Some((label, run)) = action {
            line = line.child(self.text_button(
                id,
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
        div()
            .id(id.into())
            .flex_shrink_0()
            .cursor_pointer()
            .text_size(px(TEXT_S))
            .text_color(t.accent_strong)
            .hover(|style| style.text_color(t.text1))
            .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
            .child(label.into())
            .into_any_element()
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

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if self.connection.is_open() || self.more.is_open() {
            self.connection.close();
            self.more.close();
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
            vec![
                (Page::Stacks, format!("Stacks  {stack_count}").into(), None),
                (
                    Page::Review,
                    format!("Pull requests  {review_count}").into(),
                    None,
                ),
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
            .trailing(icon(IconName::ChevronDown, 12., t.text3))
            .on_click(cx.listener(|this, _, _, cx| {
                this.connection.toggle();
                cx.notify();
            })),
        );
        if self.connection.is_open() {
            connection = connection.child(div().absolute().top(px(30.)).right_0().child(floating(
                Corner::TopRight,
                self.render_connection(window, cx),
            )));
        }
        h_flex()
            .flex_shrink_0()
            .h(px(TOOLBAR_H))
            .gap(px(8.))
            .pl(px(16.))
            .pr(px(8.))
            .border_b_1()
            .border_color(t.border2)
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(TEXT_L))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.text1)
                    .child("Publish"),
            )
            .child(
                div()
                    .min_w(px(40.))
                    .flex_shrink()
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

    fn render_connection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
        let mut panel = popover(cx)
            .swallow_clicks()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.connection.close();
                cx.notify();
            }))
            .w(px(330.))
            .p(px(16.))
            .text_size(px(TEXT_S));
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
            panel = panel.child(div().mt(px(10.)).text_color(t.danger).child(error));
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
        let color = job_tone(status).map_or(t.accent_strong, |tone| tone_color(&t, tone));
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
                    .child(dot(color, 7.))
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
                            .flex_shrink()
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
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .on_action(cx.listener(|this, _: &FindStack, window, cx| {
                if this.page == Page::Stacks {
                    this.query.update(cx, |input, _| input.focus(window));
                }
            }))
            .child(self.render_page_bar(window, cx));
        let loading = matches!(phase, ForgePhase::Idle | ForgePhase::Loading);
        let Some(snapshot) = self.snapshot.clone() else {
            return root.child(
                div().p(px(48.)).text_color(t.text2).child(
                    self.stacks_error
                        .clone()
                        .unwrap_or_else(|| "Reading your stacks…".to_owned()),
                ),
            );
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
                    Box::new(|this, _, cx| {
                        this.connection.open();
                        cx.notify();
                    }),
                )),
                cx,
            ));
        }
        if let Some(error) = self.stacks_error.clone() {
            root = root.child(self.notice(
                "notice-stacks",
                error,
                true,
                Some(("Retry", Box::new(|this, _, cx| this.refresh_selected(cx)))),
                cx,
            ));
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

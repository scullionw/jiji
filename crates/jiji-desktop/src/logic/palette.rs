//! The command palette's model: what one context offers, and how a query
//! filters and ranks it. The view runs the returned actions; nothing here
//! touches app state (the context comes in as plain values so tests can
//! drive every shape).

use jiji_core::snapshot::{GraphNode, NodeKind, RepoSnapshot, SnapshotView};
use jiji_forge::AutoLandStatus;

use super::autoland::{is_interrupted, resume_blocker};
use super::graph::RowDensity;
use super::inspect::{action_availability, resolve_compare_from, summary_of, CompareMode};
use crate::app::{DiffLayout, Section, ThemeMode, UiIntent, WorkbenchMode, SECTIONS};
use crate::logic::text::short_id;
use crate::store::RecentRepo;
use crate::theme::Scheme;
use crate::ui::IconName;

/// What runs when a row is picked. `Intent` rows hand off to the surface
/// that owns the matching UI (plan and confirm panels, layout, view mode):
/// the palette never duplicates a consequence-stating panel.
#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    ChooseRepo,
    OpenRecent(String),
    NewTab,
    CloseTab,
    ReopenTab,
    SwitchTab(u64),
    Refresh,
    FetchUpstream,
    Undo,
    NewChild(String),
    Edit(String),
    Revert(String),
    Intent(UiIntent),
    Section(Section),
    Goto(String),
    SearchHistory(Option<String>),
    Mode(ThemeMode),
    Theme(&'static str),
    SnapshotView(SnapshotView),
    ToggleSidebar,
    ToggleGraphPanel,
    AutolandStop,
    AutolandResume(String),
    InstallCli,
    UninstallCli,
}

/// What kind of change a go-to row's glyph stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphTone {
    WorkingCopy,
    Mutable,
    Immutable,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaletteItem {
    /// Stable id: the render key, and what tests name rows by.
    pub id: String,
    pub group: &'static str,
    pub title: String,
    /// Right-hand context (a change title, a repo path, a scheme).
    pub hint: Option<String>,
    /// Extra match surface that does not render.
    pub keywords: &'static str,
    pub icon: Option<IconName>,
    /// jj node glyph that go-to rows use instead of an icon.
    pub glyph: Option<(&'static str, GlyphTone)>,
    pub shortcut: Option<String>,
    pub danger: bool,
    /// Hidden until the query matches (bulk entries: themes, recents).
    pub secondary: bool,
    pub action: PaletteAction,
}

impl PaletteItem {
    fn new(
        id: impl Into<String>,
        group: &'static str,
        title: impl Into<String>,
        icon: IconName,
        action: PaletteAction,
    ) -> Self {
        Self {
            id: id.into(),
            group,
            title: title.into(),
            hint: None,
            keywords: "",
            icon: Some(icon),
            glyph: None,
            shortcut: None,
            danger: false,
            secondary: false,
            action,
        }
    }

    fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    fn keywords(mut self, keywords: &'static str) -> Self {
        self.keywords = keywords;
        self
    }

    fn shortcut(mut self, keys: impl Into<String>) -> Self {
        self.shortcut = Some(keys.into());
        self
    }

    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    fn secondary(mut self, secondary: bool) -> Self {
        self.secondary = secondary;
        self
    }
}

pub struct ThemeChoice {
    pub id: &'static str,
    pub label: &'static str,
    pub scheme: Scheme,
}

pub struct CliState<'a> {
    pub installed: bool,
    pub upgrade_available: bool,
    pub path: Option<&'a str>,
    pub conflict: bool,
}

pub struct OpenTab {
    pub id: u64,
    pub label: String,
    pub active: bool,
}

pub struct PaletteContext<'a> {
    pub snapshot: Option<&'a RepoSnapshot>,
    /// The workbench selection resolved to its node, if any.
    pub selected: Option<&'a GraphNode>,
    pub recent_repos: &'a [RecentRepo],
    /// Whether the breadcrumb's undo target is still around.
    pub can_undo: bool,
    /// Themes are a supporter perk; unregistered copies get no theme rows.
    pub registered: bool,
    pub themes: &'a [ThemeChoice],
    /// The lens presets: view, label, description.
    pub lenses: &'a [(SnapshotView, &'static str, &'static str)],
    /// Publish's landable stack heads (bookmark, title) — empty until the
    /// forge connection is verified (the land plan the rows open needs
    /// GitHub).
    pub landable: &'a [(String, String)],
    /// Publish's shippable stack heads (head change, title, change count)
    /// — empty until a GitHub remote is detected.
    pub shippable: &'a [(String, String, usize)],
    /// Someone else's pull requests on the stacks here, as (number, "#9 by
    /// @kim", title): each can be asked to be taken over.
    pub takeable: &'a [(u64, String, String)],
    /// The pull requests that were taken over here, as (number, "#9 by
    /// @kim", branch): each can be given back.
    pub taken_over: &'a [(u64, String, String)],
    /// The open repo's auto-land job records, live job first.
    pub autoland_jobs: &'a [&'a AutoLandStatus],
    /// The `jiji` terminal launcher's install state; `None` while unknown,
    /// which suppresses both rows.
    pub cli: Option<CliState<'a>>,
    /// The shell's open tabs, in strip order.
    pub open_tabs: &'a [OpenTab],
    /// The label of the closed tab ⌘⇧T would reopen.
    pub reopenable_tab: Option<&'a str>,
}

fn section_icon(section: Section) -> IconName {
    match section {
        Section::Workbench => IconName::Workbench,
        Section::Conflicts => IconName::Conflicts,
        Section::Publish => IconName::Publish,
        Section::Bookmarks => IconName::Bookmark,
        Section::Operations => IconName::Operations,
        Section::Workspaces => IconName::Workspaces,
    }
}

fn title_of(node: &GraphNode) -> &str {
    match summary_of(&node.description) {
        "" => "no description",
        title => title,
    }
}

fn item(
    id: impl Into<String>,
    group: &'static str,
    title: impl Into<String>,
    icon: IconName,
    action: PaletteAction,
) -> PaletteItem {
    PaletteItem::new(id, group, title, icon, action)
}

/// Every command this context offers, in group order. Availability mirrors
/// the surfaces the commands route to: selection actions follow the change
/// header's rules, compare presets appear only when they resolve,
/// repository commands only when they have something to act on.
fn build_items(ctx: &PaletteContext) -> Vec<PaletteItem> {
    let mut items = Vec::new();
    let intent = PaletteAction::Intent;

    if let (Some(snapshot), Some(selected)) = (ctx.snapshot, ctx.selected) {
        let avail = action_availability(snapshot, selected);
        let id4 = short_id(&selected.id, 4);
        let at = format!("{id4} \u{201c}{}\u{201d}", title_of(selected));
        if avail.describe {
            items.push(
                item(
                    "change.describe",
                    "Change",
                    if selected.description.trim().is_empty() {
                        "Describe change"
                    } else {
                        "Edit description"
                    },
                    IconName::Edit,
                    intent(UiIntent::Describe),
                )
                .hint(at.clone())
                .keywords("describe message commit text"),
            );
        }
        items.push(
            item(
                "change.new",
                "Change",
                "New child change",
                IconName::Plus,
                PaletteAction::NewChild(selected.id.clone()),
            )
            .hint(format!("on {at}"))
            .keywords("start work jj new empty"),
        );
        if avail.edit {
            items.push(
                item(
                    "change.edit",
                    "Change",
                    "Edit change",
                    IconName::AtSign,
                    PaletteAction::Edit(selected.id.clone()),
                )
                .hint(format!("make {id4} the working copy"))
                .keywords("checkout working copy jj edit"),
            );
        }
        items.push(
            item(
                "change.bookmark",
                "Change",
                "Bookmark…",
                IconName::Bookmark,
                intent(UiIntent::Bookmark),
            )
            .hint(at.clone())
            .keywords("branch create move bookmark"),
        );
        if avail.rebase {
            items.push(
                item(
                    "change.rebase",
                    "Change",
                    "Rebase…",
                    IconName::Rebase,
                    intent(UiIntent::Rebase),
                )
                .hint(at.clone())
                .keywords("move onto parent destination"),
            );
        }
        if avail.split {
            items.push(
                item(
                    "change.split",
                    "Change",
                    "Split change…",
                    IconName::Split,
                    intent(UiIntent::Split {
                        into: false,
                        path: None,
                    }),
                )
                .hint(at.clone())
                .keywords("carve separate divide files jj split"),
            );
            items.push(
                item(
                    "change.squashInto",
                    "Change",
                    "Move files into another change…",
                    IconName::Split,
                    intent(UiIntent::Split {
                        into: true,
                        path: None,
                    }),
                )
                .hint(at.clone())
                .keywords("squash into amend hunks jj squash --into"),
            );
        }
        if avail.squash {
            items.push(
                item(
                    "change.squash",
                    "Change",
                    "Squash into parent",
                    IconName::Squash,
                    intent(UiIntent::Squash),
                )
                .hint(at.clone())
                .keywords("fold combine amend meld"),
            );
        }
        // Evolution is read-only, so every selection offers it — how a
        // change was rewritten is exactly what you ask about immutable
        // history too.
        items.push(
            item(
                "change.evolog",
                "Change",
                "Show evolution",
                IconName::Operations,
                intent(UiIntent::Evolog),
            )
            .hint(at.clone())
            .keywords(
                "evolog history versions predecessors rewritten obslog restore timeline over \
                 time scrub",
            ),
        );
        // Revert backs out the change's diff as a new change on `@` —
        // works on immutable changes too (backing out landed work is the
        // point), pointless for a change with no diff.
        if !selected.is_empty {
            items.push(
                item(
                    "change.revert",
                    "Change",
                    "Revert change",
                    IconName::Undo,
                    PaletteAction::Revert(selected.id.clone()),
                )
                .hint(at.clone())
                .keywords("back out backout undo jj revert"),
            );
        }
        if avail.abandon {
            items.push(
                item(
                    "change.abandon",
                    "Change",
                    "Abandon change",
                    IconName::Trash,
                    intent(UiIntent::Abandon),
                )
                .hint(at.clone())
                .keywords("delete drop remove discard")
                .danger(),
            );
        }

        let compare = |id: &'static str, title: String, mode: Option<CompareMode>| {
            item(
                id,
                "Compare",
                title,
                IconName::Compare,
                PaletteAction::Intent(UiIntent::Compare(mode)),
            )
        };
        items.push(
            compare(
                "compare.parent",
                "Compare against parent".into(),
                Some(CompareMode::Parent),
            )
            .keywords("diff vs reset"),
        );
        if resolve_compare_from(snapshot, &selected.id, &CompareMode::Trunk).is_some() {
            let trunk = if snapshot.trunk_bookmark.is_empty() {
                "trunk"
            } else {
                &snapshot.trunk_bookmark
            };
            items.push(
                compare(
                    "compare.trunk",
                    format!("Compare against {trunk}"),
                    Some(CompareMode::Trunk),
                )
                .keywords("diff vs trunk main"),
            );
        }
        if resolve_compare_from(snapshot, &selected.id, &CompareMode::Base).is_some() {
            items.push(
                compare(
                    "compare.base",
                    "Compare against stack base".into(),
                    Some(CompareMode::Base),
                )
                .keywords("diff vs cumulative"),
            );
        }
        items.push(
            compare("compare.pick", "Compare against any change…".into(), None)
                .keywords("diff vs commit pick"),
        );
    }

    if let Some(snapshot) = ctx.snapshot {
        items.push(
            item(
                "view.graph",
                "View",
                "Graph view",
                IconName::Branch,
                intent(UiIntent::View(WorkbenchMode::Graph)),
            )
            .hint("every workstream in one tree")
            .keywords("workbench log tree")
            .shortcut("G"),
        );
        items.push(
            item(
                "view.focus",
                "View",
                "Focus view",
                IconName::Stack,
                intent(UiIntent::View(WorkbenchMode::Focus)),
            )
            .hint("one workstream at a time")
            .keywords("workbench lane stack")
            .shortcut("W"),
        );
        // The lens presets, query-only: six rows would swamp the default
        // list, and the picker in the pane head is the discoverable route.
        for (view, label, description) in ctx.lenses {
            items.push(
                item(
                    format!("lens.{label}"),
                    "View",
                    format!("Show: {label}"),
                    IconName::Eye,
                    PaletteAction::SnapshotView(view.clone()),
                )
                .hint(*description)
                .keywords("view lens graph show revset filter history")
                .secondary(true),
            );
        }
        // Row density pair, query-only like the lenses: the pane head's
        // toggle is the discoverable route.
        items.push(
            item(
                "density.compact",
                "View",
                "Compact rows",
                IconName::RowsCompact,
                intent(UiIntent::Density(RowDensity::Compact)),
            )
            .hint("jj-dense, one line per change")
            .keywords("density row size small graph")
            .secondary(true),
        );
        items.push(
            item(
                "density.classic",
                "View",
                "Classic rows",
                IconName::RowsClassic,
                intent(UiIntent::Density(RowDensity::Classic)),
            )
            .hint("larger, two lines per change")
            .keywords("density row size large big graph")
            .secondary(true),
        );
        if ctx.selected.is_some() {
            items.push(
                item(
                    "layout.unified",
                    "View",
                    "Unified diff",
                    IconName::DiffUnified,
                    intent(UiIntent::Layout(DiffLayout::Unified)),
                )
                .keywords("layout single column"),
            );
            items.push(
                item(
                    "layout.split",
                    "View",
                    "Side-by-side diff",
                    IconName::DiffSplit,
                    intent(UiIntent::Layout(DiffLayout::Split)),
                )
                .keywords("layout split two column"),
            );
            items.push(
                item(
                    "layout.filetree",
                    "View",
                    "Toggle file tree",
                    IconName::ListTree,
                    intent(UiIntent::FileTree),
                )
                .hint("navigate the diff by directory")
                .keywords("files tree explorer folders diff navigation")
                .secondary(true),
            );
        }
        items.push(
            item(
                "view.sidebar",
                "View",
                "Toggle sidebar",
                IconName::Sidebar,
                PaletteAction::ToggleSidebar,
            )
            .hint("show or hide the section rail")
            .keywords("collapse expand navigation hide rail")
            .shortcut("⌘B"),
        );
        items.push(
            item(
                "view.graphpanel",
                "View",
                "Toggle graph panel",
                IconName::PanelClose,
                PaletteAction::ToggleGraphPanel,
            )
            .hint("full-width diff")
            .keywords("collapse expand hide show log pane maximize zen")
            .shortcut("⌘⇧B"),
        );
        // The auto-land jobs and the land-a-stack routes (queue and stop
        // without the mouse). Stop and resume are the job cards' one-click
        // actions; the stack rows hand off to Publish's plan card — the
        // consequence-stating panel owning Land and Auto-land — exactly
        // like a Change row opens its plan panel.
        for job in ctx.autoland_jobs {
            let bookmark = &job.record.state.head_bookmark;
            if job.live {
                items.push(
                    item(
                        "autoland.stop",
                        "Publish",
                        "Stop auto-land job",
                        IconName::Close,
                        PaletteAction::AutolandStop,
                    )
                    .hint(bookmark.clone())
                    .keywords("cancel halt watch queue auto-land landing"),
                );
            } else if is_interrupted(job)
                // Admin-bypass resumes require the explicit warning and
                // confirmation on Publish's job card; a compact palette
                // row must not make those saved rules invisible again.
                && !job.record.state.options.bypass
                && resume_blocker(job, &snapshot.bookmarks).is_none()
            {
                items.push(
                    item(
                        format!("autoland.resume.{bookmark}"),
                        "Publish",
                        format!("Resume auto-land job for {bookmark}"),
                        IconName::Refresh,
                        PaletteAction::AutolandResume(bookmark.clone()),
                    )
                    .hint("interrupted")
                    .keywords("continue restart watch queue auto-land landing"),
                );
            }
        }
        for (bookmark, title) in ctx.landable {
            items.push(
                item(
                    format!("publish.land.{bookmark}"),
                    "Publish",
                    format!("Land {bookmark}…"),
                    IconName::Publish,
                    intent(UiIntent::Land {
                        bookmark: bookmark.clone(),
                    }),
                )
                .hint(title.clone())
                .keywords("auto-land queue merge publish stack pull request"),
            );
        }
        for (head, title, count) in ctx.shippable {
            items.push(
                item(
                    format!("publish.ship.{head}"),
                    "Publish",
                    format!("Ship \u{201c}{title}\u{201d} to trunk…"),
                    IconName::Publish,
                    intent(UiIntent::Ship { head: head.clone() }),
                )
                .hint(format!(
                    "{} · {count} change{}",
                    short_id(head, 8),
                    if *count == 1 { "" } else { "s" }
                ))
                .keywords("ship now push trunk direct no pr main"),
            );
        }
        // Someone else's pull request on a stack here, and the way back
        // for one that was taken over. Both rows hand off to Publish's
        // card, which says what will happen and waits to be confirmed.
        for (number, who, title) in ctx.takeable {
            items.push(
                item(
                    format!("publish.takeOver.{number}"),
                    "Publish",
                    format!("Take over {who}…"),
                    IconName::Branch,
                    intent(UiIntent::TakeOver { number: *number }),
                )
                .hint(title.clone())
                .keywords("takeover adopt someone else's pull request branch track push finish"),
            );
        }
        for (number, who, branch) in ctx.taken_over {
            items.push(
                item(
                    format!("publish.giveBack.{number}"),
                    "Publish",
                    format!("Give back {who}…"),
                    IconName::Branch,
                    intent(UiIntent::GiveBack { number: *number }),
                )
                .hint(branch.clone())
                .keywords("hand back return release taken over pull request branch untrack"),
            );
        }
        // Back to the working copy from anywhere in history — only while
        // the lens draws it.
        if let Some(working_copy) = snapshot
            .nodes
            .iter()
            .find(|node| node.id == snapshot.working_copy)
        {
            items.push(
                item(
                    "navigate.workingCopy",
                    "Navigate",
                    "Go to working copy",
                    IconName::AtSign,
                    PaletteAction::Goto(working_copy.id.clone()),
                )
                .hint(title_of(working_copy))
                .keywords("@ jump current checkout wc")
                .shortcut("@"),
            );
        }
        for (index, section) in SECTIONS.into_iter().enumerate() {
            items.push(
                item(
                    format!("section.{}", section.label()),
                    "Navigate",
                    format!("Go to {}", section.label()),
                    section_icon(section),
                    PaletteAction::Section(section),
                )
                .shortcut(format!("⌘{}", index + 1)),
            );
        }
        items.push(
            item(
                "search.history",
                "Navigate",
                "Search history…",
                IconName::Search,
                PaletteAction::SearchHistory(None),
            )
            .hint("find a string or filename across changes")
            .keywords("search find grep pickaxe text content file log -S history")
            .shortcut("⌘⇧F"),
        );
    }

    items.push(
        item(
            "repo.open",
            "Repository",
            "Open repository…",
            IconName::Folder,
            PaletteAction::ChooseRepo,
        )
        .keywords("folder project jj git repo")
        .shortcut("⌘O"),
    );
    // Tabs: switch rows for every other open repo, and the strip's own
    // commands so the keyboard route covers them too.
    for tab in ctx.open_tabs.iter().filter(|tab| !tab.active) {
        items.push(
            item(
                format!("tab.switch.{}", tab.id),
                "Repository",
                format!("Switch to {}", tab.label),
                IconName::Folder,
                PaletteAction::SwitchTab(tab.id),
            )
            .keywords("tab switch repository open window"),
        );
    }
    items.push(
        item(
            "tab.new",
            "Repository",
            "New tab",
            IconName::Plus,
            PaletteAction::NewTab,
        )
        .keywords("tab open empty")
        .shortcut("⌘T")
        .secondary(true),
    );
    if ctx.open_tabs.len() > 1 || ctx.snapshot.is_some() {
        items.push(
            item(
                "tab.close",
                "Repository",
                "Close tab",
                IconName::Close,
                PaletteAction::CloseTab,
            )
            .keywords("tab close repository")
            .shortcut("⌘W")
            .secondary(true),
        );
    }
    if let Some(label) = ctx.reopenable_tab {
        items.push(
            item(
                "tab.reopen",
                "Repository",
                "Reopen closed tab",
                IconName::Undo,
                PaletteAction::ReopenTab,
            )
            .hint(label)
            .keywords("tab undo close restore repository")
            .shortcut("⌘⇧T")
            .secondary(true),
        );
    }
    let open_path = ctx.snapshot.map(|snapshot| snapshot.repo_path.as_str());
    for recent in ctx.recent_repos.iter().take(5) {
        if Some(recent.path.as_str()) == open_path {
            continue;
        }
        items.push(
            item(
                format!("repo.recent.{}", recent.path),
                "Repository",
                format!("Open {}", recent.name),
                IconName::Folder,
                PaletteAction::OpenRecent(recent.path.clone()),
            )
            .hint(recent.path.clone())
            .keywords("recent repository switch")
            // Front and center on the welcome screen; out of the way once
            // a repo is open.
            .secondary(ctx.snapshot.is_some()),
        );
    }
    if let Some(snapshot) = ctx.snapshot {
        items.push(
            item(
                "repo.refresh",
                "Repository",
                "Refresh snapshot",
                IconName::Refresh,
                PaletteAction::Refresh,
            )
            .keywords("reload sync update")
            .shortcut("⌘R"),
        );
        if !snapshot.git_remotes.is_empty() {
            items.push(
                item(
                    "repo.fetch",
                    "Repository",
                    "Fetch from remotes",
                    IconName::Refresh,
                    PaletteAction::FetchUpstream,
                )
                .keywords("git fetch upstream pull remote sync check"),
            );
        }
    }
    if ctx.can_undo {
        items.push(
            item(
                "repo.undo",
                "Repository",
                "Undo last operation",
                IconName::Undo,
                PaletteAction::Undo,
            )
            .keywords("revert rollback mistake"),
        );
    }
    // The terminal launcher. The install row mirrors the welcome card's
    // offer (and stays visible there even after a "not now"); once a repo
    // is open it goes query-only, like removal always is. A foreign `jiji`
    // on PATH suppresses both — installing would clobber it.
    if let Some(cli) = &ctx.cli {
        if (!cli.installed || cli.upgrade_available) && !cli.conflict {
            items.push(
                item(
                    "cli.install",
                    "Repository",
                    if cli.upgrade_available {
                        "Update jiji command-line tools"
                    } else {
                        "Install jiji command-line tools"
                    },
                    IconName::Terminal,
                    PaletteAction::InstallCli,
                )
                .hint("publish, land, and open repos from a terminal")
                .keywords("cli shell terminal command install update upgrade launcher path")
                .secondary(ctx.snapshot.is_some()),
            );
        }
        if cli.installed {
            let mut row = item(
                "cli.uninstall",
                "Repository",
                "Remove jiji command-line launcher",
                IconName::Terminal,
                PaletteAction::UninstallCli,
            )
            .keywords("cli shell terminal command uninstall delete launcher")
            .secondary(true);
            row.hint = cli.path.map(str::to_owned);
            items.push(row);
        }
    }

    // Theme rows are supporter-gated (like the picker) and query-only: ten
    // palettes would swamp the default list.
    if ctx.registered {
        for (mode, label, glyph) in [
            (ThemeMode::System, "follow system", IconName::SunMoon),
            (ThemeMode::Light, "light", IconName::Sun),
            (ThemeMode::Dark, "dark", IconName::Moon),
        ] {
            items.push(
                item(
                    format!("appearance.{label}"),
                    "Appearance",
                    format!("Appearance: {label}"),
                    glyph,
                    PaletteAction::Mode(mode),
                )
                .keywords("theme mode color scheme")
                .secondary(true),
            );
        }
        for theme in ctx.themes {
            items.push(
                item(
                    format!("theme.{}", theme.id),
                    "Appearance",
                    format!("Theme: {}", theme.label),
                    IconName::Sparkles,
                    PaletteAction::Theme(theme.id),
                )
                .hint(match theme.scheme {
                    Scheme::Dark => "dark",
                    Scheme::Light => "light",
                })
                .keywords("theme color palette appearance")
                .secondary(true),
            );
        }
    }

    items
}

fn tokens_of(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// How well one item matches the query: every token must land somewhere;
/// title hits outrank keyword and hint hits. 0 = filtered out.
pub fn score_item(item: &PaletteItem, tokens: &[String]) -> u32 {
    let title = item.title.to_lowercase();
    let rest = format!("{} {}", item.keywords, item.hint.as_deref().unwrap_or("")).to_lowercase();
    let mut score = 0;
    for token in tokens {
        score += if title.starts_with(token.as_str()) {
            4
        } else if title
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word.starts_with(token.as_str()))
        {
            3
        } else if title.contains(token.as_str()) {
            2
        } else if rest.contains(token.as_str()) {
            1
        } else {
            return 0;
        };
    }
    score
}

const GOTO_LIMIT: usize = 8;

/// Changes the query names — by id, change-id or commit-id prefix, title
/// text, or bookmark — as jump rows. Query-only, capped, in graph order.
pub fn goto_matches(
    snapshot: &RepoSnapshot,
    selected: Option<&str>,
    tokens: &[String],
) -> Vec<PaletteItem> {
    let mut items = Vec::new();
    for node in &snapshot.nodes {
        if Some(node.id.as_str()) == selected {
            continue;
        }
        let ids = [&node.id, &node.change_id, &node.commit_id].map(|id| id.to_lowercase());
        let text = title_of(node).to_lowercase();
        let marks: Vec<String> = node.bookmarks.iter().map(|b| b.to_lowercase()).collect();
        let mut score = 0;
        for token in tokens {
            let token = token.as_str();
            score += if ids.iter().any(|id| id.starts_with(token)) {
                4
            } else if marks.iter().any(|mark| mark.starts_with(token)) {
                3
            } else if text.split_whitespace().any(|word| word.starts_with(token)) {
                2
            } else if text.contains(token) || marks.iter().any(|mark| mark.contains(token)) {
                1
            } else {
                score = 0;
                break;
            };
        }
        if score == 0 {
            continue;
        }
        let mut hint = vec![title_of(node).to_owned()];
        hint.extend(node.bookmarks.iter().cloned());
        let (glyph, tone) = match node.kind {
            NodeKind::WorkingCopy => ("@", GlyphTone::WorkingCopy),
            NodeKind::Mutable => ("○", GlyphTone::Mutable),
            NodeKind::Immutable => ("◆", GlyphTone::Immutable),
        };
        items.push(PaletteItem {
            id: format!("goto.{}", node.id),
            group: "Go to",
            title: short_id(&node.id, 8).to_owned(),
            hint: Some(hint.join(" · ")),
            keywords: "",
            icon: None,
            glyph: Some((if node.has_conflict { "×" } else { glyph }, tone)),
            shortcut: None,
            danger: false,
            secondary: false,
            action: PaletteAction::Goto(node.id.clone()),
        });
        if items.len() == GOTO_LIMIT {
            break;
        }
    }
    items
}

/// The palette's contents for one context and query. Empty query: the
/// primary commands in group order. With a query: every match (commands
/// and go-to-change rows) ranked best-first as one flat list, with a
/// history-search hand-off as the constant last resort — whatever was
/// typed, the pickaxe can chase it through history.
pub fn palette_results(ctx: &PaletteContext, query: &str) -> Vec<PaletteItem> {
    let items = build_items(ctx);
    let tokens = tokens_of(query);
    if tokens.is_empty() {
        return items.into_iter().filter(|item| !item.secondary).collect();
    }
    let mut scored: Vec<(u32, usize, PaletteItem)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let score = score_item(&item, &tokens);
            (score > 0).then_some((score, index, item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut results: Vec<PaletteItem> = scored.into_iter().map(|(_, _, item)| item).collect();
    if let Some(snapshot) = ctx.snapshot {
        results.extend(goto_matches(
            snapshot,
            ctx.selected.map(|node| node.id.as_str()),
            &tokens,
        ));
        let query = query.trim();
        results.push(PaletteItem::new(
            "search.query",
            "Search",
            format!("Search history for \u{201c}{query}\u{201d}"),
            IconName::Search,
            PaletteAction::SearchHistory(Some(query.to_owned())),
        ));
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiji_core::{MockBackend, RepoBackend};

    fn snapshot() -> RepoSnapshot {
        let dir = std::env::temp_dir().join(format!("jiji-palette-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".jj"));
        MockBackend::default().open(&dir).unwrap()
    }

    fn context<'a>(
        snapshot: Option<&'a RepoSnapshot>,
        selected: Option<&'a GraphNode>,
    ) -> PaletteContext<'a> {
        PaletteContext {
            snapshot,
            selected,
            recent_repos: &[],
            can_undo: false,
            registered: false,
            themes: &[],
            lenses: &[],
            landable: &[],
            shippable: &[],
            takeable: &[],
            taken_over: &[],
            autoland_jobs: &[],
            cli: None,
            open_tabs: &[],
            reopenable_tab: None,
        }
    }

    fn ids(items: &[PaletteItem]) -> Vec<&str> {
        items.iter().map(|item| item.id.as_str()).collect()
    }

    #[test]
    fn the_welcome_screen_offers_repositories_only() {
        let recents = [RecentRepo {
            path: "/work/alpha".into(),
            name: "alpha".into(),
            last_opened_at: 1,
        }];
        let ctx = PaletteContext {
            recent_repos: &recents,
            ..context(None, None)
        };
        assert_eq!(
            ids(&palette_results(&ctx, "")),
            ["repo.open", "repo.recent./work/alpha"]
        );
        // No repo: no history to search, so no hand-off row either.
        assert_eq!(
            ids(&palette_results(&ctx, "alp")),
            ["repo.recent./work/alpha"]
        );
    }

    #[test]
    fn selection_commands_follow_what_the_change_allows() {
        let snapshot = snapshot();
        let immutable = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Immutable)
            .unwrap();
        let listed = palette_results(&context(Some(&snapshot), Some(immutable)), "");
        let listed = ids(&listed);
        assert!(listed.contains(&"change.new") && listed.contains(&"change.evolog"));
        for rewrite in [
            "change.describe",
            "change.rebase",
            "change.abandon",
            "change.split",
        ] {
            assert!(
                !listed.contains(&rewrite),
                "{rewrite} offered on an immutable change"
            );
        }
        let mutable = snapshot
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Mutable && !n.is_empty)
            .unwrap();
        let listed = palette_results(&context(Some(&snapshot), Some(mutable)), "");
        let listed = ids(&listed);
        for id in [
            "change.describe",
            "change.rebase",
            "change.abandon",
            "compare.parent",
        ] {
            assert!(listed.contains(&id), "{id} missing");
        }
        // Bulk rows stay out of the browsable list.
        assert!(!listed.contains(&"density.compact") && !listed.contains(&"tab.new"));
    }

    #[test]
    fn a_query_ranks_title_hits_first_and_ends_with_the_search_handoff() {
        let snapshot = snapshot();
        let ctx = context(Some(&snapshot), None);
        let results = palette_results(&ctx, "refresh");
        assert_eq!(results[0].id, "repo.refresh");
        assert_eq!(results.last().unwrap().id, "search.query");
        assert_eq!(
            results.last().unwrap().action,
            PaletteAction::SearchHistory(Some("refresh".into()))
        );
        // Equal scores keep the list's own order.
        assert_eq!(
            ids(&palette_results(&ctx, "tab")),
            ["tab.new", "tab.close", "search.query"]
        );
    }

    #[test]
    fn scoring_needs_every_token() {
        let item = PaletteItem::new(
            "x",
            "View",
            "Toggle graph panel",
            IconName::Eye,
            PaletteAction::Refresh,
        )
        .keywords("collapse zen");
        let tokens = |text: &str| tokens_of(text);
        assert_eq!(score_item(&item, &tokens("toggle")), 4);
        assert_eq!(score_item(&item, &tokens("graph")), 3);
        assert_eq!(score_item(&item, &tokens("ogg")), 2);
        assert_eq!(score_item(&item, &tokens("zen")), 1);
        assert_eq!(score_item(&item, &tokens("toggle zen")), 5);
        assert_eq!(score_item(&item, &tokens("toggle nope")), 0);
    }

    #[test]
    fn changes_are_reachable_by_id_title_or_bookmark() {
        let snapshot = snapshot();
        let node = snapshot
            .nodes
            .iter()
            .find(|n| !n.bookmarks.is_empty())
            .unwrap();
        let by_id = goto_matches(&snapshot, None, &tokens_of(short_id(&node.id, 4)));
        assert!(by_id
            .iter()
            .any(|item| item.action == PaletteAction::Goto(node.id.clone())));
        let by_mark = goto_matches(&snapshot, None, &tokens_of(&node.bookmarks[0]));
        assert!(by_mark
            .iter()
            .any(|item| item.action == PaletteAction::Goto(node.id.clone())));
        // The selection itself is not a place to go.
        let skipping = goto_matches(&snapshot, Some(&node.id), &tokens_of(short_id(&node.id, 4)));
        assert!(skipping
            .iter()
            .all(|item| item.action != PaletteAction::Goto(node.id.clone())));
        assert!(goto_matches(&snapshot, None, &tokens_of("zzzzqqqq")).is_empty());
    }

    #[test]
    fn themes_are_for_registered_copies_and_only_on_query() {
        let themes = [ThemeChoice {
            id: "moss",
            label: "Moss",
            scheme: Scheme::Dark,
        }];
        let unregistered = PaletteContext {
            themes: &themes,
            ..context(None, None)
        };
        assert!(palette_results(&unregistered, "moss").is_empty());
        let registered = PaletteContext {
            registered: true,
            ..unregistered
        };
        assert_eq!(ids(&palette_results(&registered, "moss")), ["theme.moss"]);
        assert!(!ids(&palette_results(&registered, "")).contains(&"theme.moss"));
    }

    // ── Ported from the Tauri app's palette.test.ts ──

    use jiji_core::snapshot::{BookmarkState, GitRemote, SyncState, WorkstreamSummary};
    use jiji_forge::{AutoLandPhase, AutoLandRecord, AutoLandState};

    fn change(id: &str, kind: NodeKind, parents: &[&str], bookmarks: &[&str]) -> GraphNode {
        GraphNode {
            id: id.into(),
            change_id: id.into(),
            commit_id: format!("c{id}"),
            description: format!("change {id}"),
            author: "test".into(),
            timestamp: "2026-06-10T12:00:00Z".into(),
            kind,
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            elided_parents: vec![],
            bookmarks: bookmarks.iter().map(|b| (*b).to_owned()).collect(),
            tags: vec![],
            is_empty: false,
            has_conflict: false,
            is_divergent: false,
            held: false,
            old_copies_held: false,
        }
    }

    fn local(name: &str, target: &str, trunk: bool) -> BookmarkState {
        BookmarkState {
            name: name.into(),
            target: target.into(),
            remote: None,
            sync: SyncState::LocalOnly,
            is_trunk: trunk,
            is_local: true,
            tracked: false,
            is_merged: false,
            target_description: String::new(),
            target_author: String::new(),
            target_timestamp: String::new(),
        }
    }

    /// A stack on trunk: wxyz → mnop (feature) → qrst → base (main).
    fn stack() -> RepoSnapshot {
        RepoSnapshot {
            repo_path: "/tmp/repo".into(),
            working_copy: String::new(),
            trunk_bookmark: "main".into(),
            git_remotes: vec![],
            nodes: vec![
                change("wxyz", NodeKind::WorkingCopy, &["mnop"], &[]),
                change("mnop", NodeKind::Mutable, &["qrst"], &["feature"]),
                change("qrst", NodeKind::Mutable, &["base"], &[]),
                change("base", NodeKind::Immutable, &[], &["main"]),
            ],
            workstreams: vec![WorkstreamSummary {
                id: "ws".into(),
                title: "ws".into(),
                node_ids: vec!["wxyz".into(), "mnop".into(), "qrst".into()],
                bookmark: None,
                is_active: false,
                behind_trunk: 0,
            }],
            bookmarks: vec![local("main", "base", true), local("feature", "mnop", false)],
            ..snapshot()
        }
    }

    fn listed(ctx: &PaletteContext, query: &str) -> Vec<String> {
        palette_results(ctx, query)
            .into_iter()
            .map(|item| item.id)
            .collect()
    }

    fn has(list: &[String], id: &str) -> bool {
        list.iter().any(|listed| listed == id)
    }

    #[test]
    fn a_mutable_change_on_a_mutable_parent_gets_every_command() {
        let snapshot = stack();
        let ctx = context(Some(&snapshot), Some(&snapshot.nodes[1]));
        let items = listed(&ctx, "");
        for id in [
            "change.describe",
            "change.new",
            "change.edit",
            "change.bookmark",
            "change.rebase",
            "change.split",
            "change.squash",
            "change.revert",
            "change.evolog",
            "change.abandon",
            "compare.parent",
            "compare.trunk",
            "compare.base",
            "compare.pick",
            "view.graph",
            "view.focus",
            "layout.unified",
            "layout.split",
            "section.Operations",
            "repo.open",
            "repo.refresh",
        ] {
            assert!(has(&items, id), "{id} missing");
        }
    }

    #[test]
    fn an_immutable_change_gets_what_rewrites_nothing_revert_and_history_too() {
        let snapshot = stack();
        let items = listed(&context(Some(&snapshot), Some(&snapshot.nodes[3])), "");
        for id in [
            "change.new",
            "change.bookmark",
            "change.revert",
            "change.evolog",
        ] {
            assert!(has(&items, id), "{id} missing");
        }
        for id in [
            "change.describe",
            "change.edit",
            "change.rebase",
            "change.split",
            "change.squash",
            "change.abandon",
        ] {
            assert!(!has(&items, id), "{id} offered");
        }
    }

    #[test]
    fn revert_squash_and_edit_are_withheld_where_they_mean_nothing() {
        let snapshot = stack();
        let mut empty = snapshot.nodes[1].clone();
        empty.is_empty = true;
        assert!(!has(
            &listed(&context(Some(&snapshot), Some(&empty)), ""),
            "change.revert"
        ));
        // qrst sits right on the immutable base.
        assert!(!has(
            &listed(&context(Some(&snapshot), Some(&snapshot.nodes[2])), ""),
            "change.squash"
        ));
        // The working copy is edited already.
        let items = listed(&context(Some(&snapshot), Some(&snapshot.nodes[0])), "");
        assert!(!has(&items, "change.edit") && has(&items, "change.squash"));
    }

    #[test]
    fn a_comparison_that_cannot_resolve_is_not_offered() {
        // No trunk bookmark, no stream claiming the change.
        let bare = RepoSnapshot {
            workstreams: vec![],
            bookmarks: vec![],
            trunk_bookmark: String::new(),
            ..stack()
        };
        let items = listed(&context(Some(&bare), Some(&bare.nodes[1])), "");
        assert!(!has(&items, "compare.trunk") && !has(&items, "compare.base"));
        assert!(has(&items, "compare.parent") && has(&items, "compare.pick"));
    }

    #[test]
    fn undo_is_offered_while_its_operation_is_around() {
        let snapshot = stack();
        let ctx = context(Some(&snapshot), Some(&snapshot.nodes[1]));
        assert!(!has(&listed(&ctx, ""), "repo.undo"));
        let undoable = PaletteContext {
            can_undo: true,
            ..ctx
        };
        assert!(has(&listed(&undoable, ""), "repo.undo"));
    }

    #[test]
    fn the_other_tabs_are_rows_and_tab_commands_answer_a_query() {
        let tabs = [
            OpenTab {
                id: 1,
                label: "alpha".into(),
                active: true,
            },
            OpenTab {
                id: 2,
                label: "beta".into(),
                active: false,
            },
        ];
        let ctx = PaletteContext {
            open_tabs: &tabs,
            ..context(None, None)
        };
        let items = listed(&ctx, "");
        assert!(has(&items, "tab.switch.2") && !has(&items, "tab.switch.1"));
        let queried = listed(&ctx, "tab");
        assert!(has(&queried, "tab.new") && has(&queried, "tab.close"));
    }

    #[test]
    fn reopening_a_closed_tab_answers_a_query_and_names_it() {
        assert!(!has(&listed(&context(None, None), "reopen"), "tab.reopen"));
        let ctx = PaletteContext {
            reopenable_tab: Some("beta"),
            ..context(None, None)
        };
        let results = palette_results(&ctx, "reopen");
        let row = results.iter().find(|item| item.id == "tab.reopen").unwrap();
        assert_eq!(row.hint.as_deref(), Some("beta"));
        assert_eq!(row.action, PaletteAction::ReopenTab);
        assert!(!has(&listed(&ctx, ""), "tab.reopen"));
    }

    #[test]
    fn fetching_upstream_needs_a_git_remote() {
        let snapshot = stack();
        assert!(!has(
            &listed(&context(Some(&snapshot), None), ""),
            "repo.fetch"
        ));
        let remoted = RepoSnapshot {
            git_remotes: vec![GitRemote {
                name: "origin".into(),
                url: "https://example.com/r.git".into(),
            }],
            ..stack()
        };
        assert!(has(
            &listed(&context(Some(&remoted), None), ""),
            "repo.fetch"
        ));
    }

    #[test]
    fn each_landable_and_shippable_stack_is_a_row_opening_its_plan() {
        let snapshot = stack();
        let landable = [("feature".to_owned(), "ws".to_owned())];
        let shippable = [("mnop".to_owned(), "feat: work".to_owned(), 2)];
        let ctx = PaletteContext {
            landable: &landable,
            shippable: &shippable,
            ..context(Some(&snapshot), Some(&snapshot.nodes[1]))
        };
        let results = palette_results(&ctx, "");
        let land = results
            .iter()
            .find(|item| item.id == "publish.land.feature")
            .unwrap();
        assert_eq!(
            land.action,
            PaletteAction::Intent(UiIntent::Land {
                bookmark: "feature".into()
            })
        );
        let results = palette_results(&ctx, "ship");
        let ship = results
            .iter()
            .find(|item| item.id == "publish.ship.mnop")
            .unwrap();
        assert_eq!(
            ship.action,
            PaletteAction::Intent(UiIntent::Ship {
                head: "mnop".into()
            })
        );
        assert!(ship.title.contains("feat: work"));
        // With nothing passed in (no connection, no GitHub remote), none.
        let bare = listed(&context(Some(&snapshot), Some(&snapshot.nodes[1])), "ship");
        assert!(!has(&bare, "publish.land.feature") && !has(&bare, "publish.ship.mnop"));
    }

    /// Someone else's pull request on a stack here is a row that opens
    /// the plan to take it over, found by what the user would type; one
    /// that was taken over has the row that gives it back.
    #[test]
    fn taking_over_and_giving_back_are_rows_that_open_their_plan() {
        let snapshot = stack();
        let takeable = [(9, "#9 by @kim".to_owned(), "A helping change".to_owned())];
        let taken_over = [(7, "#7 by @sam".to_owned(), "sam/fix".to_owned())];
        let ctx = PaletteContext {
            takeable: &takeable,
            taken_over: &taken_over,
            ..context(Some(&snapshot), Some(&snapshot.nodes[1]))
        };
        let results = palette_results(&ctx, "take over");
        let take = results
            .iter()
            .find(|item| item.id == "publish.takeOver.9")
            .expect("found by the words the user would type");
        assert_eq!(take.title, "Take over #9 by @kim\u{2026}");
        assert_eq!(
            take.action,
            PaletteAction::Intent(UiIntent::TakeOver { number: 9 })
        );
        let results = palette_results(&ctx, "give back");
        let give = results
            .iter()
            .find(|item| item.id == "publish.giveBack.7")
            .expect("found by the words the user would type");
        assert_eq!(give.title, "Give back #7 by @sam\u{2026}");
        assert_eq!(
            give.action,
            PaletteAction::Intent(UiIntent::GiveBack { number: 7 })
        );
        // Nobody else on the stacks, nothing taken over: no such rows.
        let bare = listed(
            &context(Some(&snapshot), Some(&snapshot.nodes[1])),
            "take over",
        );
        assert!(
            !bare.iter().any(|id| id.starts_with("publish.takeOver")),
            "{bare:?}"
        );
    }

    fn job(head: &str, phase: AutoLandPhase, live: bool, bypass: bool) -> AutoLandStatus {
        let mut state = AutoLandState::queued(head);
        state.phase = phase;
        state.options.bypass = bypass;
        AutoLandStatus {
            live,
            record: AutoLandRecord {
                repo_path: "/tmp/repo".into(),
                state,
                saved_at_ms: 0,
            },
        }
    }

    fn watching() -> AutoLandPhase {
        AutoLandPhase::Waiting {
            attention: false,
            reasons: vec!["waiting on checks".into()],
        }
    }

    fn with_jobs(jobs: &[AutoLandStatus]) -> Vec<String> {
        let snapshot = stack();
        let jobs: Vec<&AutoLandStatus> = jobs.iter().collect();
        let ctx = PaletteContext {
            autoland_jobs: &jobs,
            ..context(Some(&snapshot), Some(&snapshot.nodes[1]))
        };
        listed(&ctx, "")
    }

    #[test]
    fn a_live_job_offers_stop_and_an_interrupted_one_resume() {
        let live = with_jobs(&[job("feature", watching(), true, false)]);
        assert!(has(&live, "autoland.stop") && !has(&live, "autoland.resume.feature"));
        assert!(!has(&with_jobs(&[]), "autoland.stop"));
        let interrupted = with_jobs(&[job("feature", watching(), false, false)]);
        assert!(has(&interrupted, "autoland.resume.feature"));
        assert!(!has(&interrupted, "autoland.stop"));
        // Each record has its own row.
        let both = with_jobs(&[
            job("feature", watching(), true, false),
            job("main", watching(), false, false),
        ]);
        assert!(has(&both, "autoland.stop") && has(&both, "autoland.resume.main"));
    }

    #[test]
    fn resume_is_withheld_for_a_bypass_a_vanished_stack_or_a_job_that_ended() {
        // Publish's card shows a saved bypass before it resumes; a palette
        // row would hide it.
        assert!(!has(
            &with_jobs(&[job("feature", watching(), false, true)]),
            "autoland.resume.feature"
        ));
        assert!(!has(
            &with_jobs(&[job("vanished", watching(), false, false)]),
            "autoland.resume.vanished"
        ));
        let done = with_jobs(&[job("feature", AutoLandPhase::Done, false, false)]);
        assert!(!has(&done, "autoland.resume.feature") && !has(&done, "autoland.stop"));
    }

    #[test]
    fn secondary_rows_wait_for_a_query_that_finds_them() {
        let snapshot = stack();
        let recents = [RecentRepo {
            path: "/tmp/other".into(),
            name: "other".into(),
            last_opened_at: 1,
        }];
        let ctx = PaletteContext {
            recent_repos: &recents,
            ..context(Some(&snapshot), Some(&snapshot.nodes[1]))
        };
        assert!(!has(&listed(&ctx, ""), "repo.recent./tmp/other"));
        assert!(has(&listed(&ctx, "other"), "repo.recent./tmp/other"));
        // The row-density pair too.
        let items = listed(&ctx, "");
        assert!(!has(&items, "density.compact") && !has(&items, "density.classic"));
        let rows = listed(&ctx, "rows");
        assert!(has(&rows, "density.compact") && has(&rows, "density.classic"));
    }

    #[test]
    fn a_title_hit_outranks_a_keyword_hit() {
        let snapshot = stack();
        let items = listed(&context(Some(&snapshot), Some(&snapshot.nodes[1])), "diff");
        assert_eq!(items[0], "layout.unified");
        let at = |id: &str| items.iter().position(|item| item == id).unwrap();
        assert!(at("compare.parent") > at("layout.split"));
    }

    #[test]
    fn a_query_nothing_answers_leaves_only_the_history_search() {
        let snapshot = stack();
        let ctx = context(Some(&snapshot), Some(&snapshot.nodes[1]));
        assert_eq!(listed(&ctx, "zzzz-no-such"), ["search.query"]);
        assert!(has(&listed(&ctx, ""), "search.history"));
        assert!(!has(&listed(&context(None, None), ""), "search.history"));
        let results = palette_results(&ctx, "  needle text ");
        let last = results.last().unwrap();
        assert_eq!(last.id, "search.query");
        assert_eq!(last.title, "Search history for \u{201c}needle text\u{201d}");
        assert_eq!(
            last.action,
            PaletteAction::SearchHistory(Some("needle text".into()))
        );
        assert!(!has(
            &listed(&context(None, None), "needle"),
            "search.query"
        ));
    }

    fn cli(installed: bool, upgrade: bool, conflict: bool) -> Option<CliState<'static>> {
        Some(CliState {
            installed,
            upgrade_available: upgrade,
            path: installed.then_some("/usr/local/bin/jiji"),
            conflict,
        })
    }

    #[test]
    fn the_command_line_tools_install_from_the_welcome_screen_or_by_name() {
        let welcome = PaletteContext {
            cli: cli(false, false, false),
            ..context(None, None)
        };
        assert!(has(&listed(&welcome, ""), "cli.install"));
        let snapshot = stack();
        let open = PaletteContext {
            cli: cli(false, false, false),
            ..context(Some(&snapshot), None)
        };
        assert!(!has(&listed(&open, ""), "cli.install"));
        assert!(has(&listed(&open, "cli"), "cli.install"));
    }

    #[test]
    fn installed_tools_offer_removal_or_an_update_of_the_old_launcher() {
        let installed = PaletteContext {
            cli: cli(true, false, false),
            ..context(None, None)
        };
        assert!(!has(&listed(&installed, ""), "cli.install"));
        assert!(!has(&listed(&installed, "install"), "cli.install"));
        assert!(has(&listed(&installed, "remove"), "cli.uninstall"));
        let old = PaletteContext {
            cli: cli(true, true, false),
            ..context(None, None)
        };
        let results = palette_results(&old, "update jiji");
        let row = results
            .iter()
            .find(|item| item.id == "cli.install")
            .unwrap();
        assert_eq!(row.title, "Update jiji command-line tools");
    }

    #[test]
    fn a_foreign_jiji_or_an_unknown_state_offers_neither() {
        let foreign = PaletteContext {
            cli: cli(false, false, true),
            ..context(None, None)
        };
        assert!(listed(&foreign, "cli").is_empty());
        assert!(listed(&context(None, None), "cli").is_empty());
    }

    #[test]
    fn a_change_is_found_by_id_bookmark_or_title_but_never_the_selection() {
        let snapshot = stack();
        let selected = context(Some(&snapshot), Some(&snapshot.nodes[1]));
        assert!(has(&listed(&selected, "qrst"), "goto.qrst"));
        // "feature" names the selection itself.
        assert!(!has(&listed(&selected, "feature"), "goto.mnop"));
        let elsewhere = context(Some(&snapshot), Some(&snapshot.nodes[0]));
        assert!(has(&listed(&elsewhere, "feature"), "goto.mnop"));
        assert!(has(&listed(&elsewhere, "change qrst"), "goto.qrst"));
    }

    #[test]
    fn going_back_to_the_working_copy_is_offered_while_the_lens_draws_it() {
        let drawn = RepoSnapshot {
            working_copy: "wxyz".into(),
            ..stack()
        };
        let results = palette_results(&context(Some(&drawn), Some(&drawn.nodes[1])), "");
        let row = results
            .iter()
            .find(|item| item.id == "navigate.workingCopy")
            .unwrap();
        assert_eq!(row.action, PaletteAction::Goto("wxyz".into()));
        assert_eq!(row.shortcut.as_deref(), Some("@"));
        let hidden = RepoSnapshot {
            working_copy: "hidden".into(),
            ..stack()
        };
        assert!(!has(
            &listed(&context(Some(&hidden), Some(&hidden.nodes[1])), ""),
            "navigate.workingCopy"
        ));
    }

    #[test]
    fn jump_rows_are_capped_and_keep_the_snapshot_s_order() {
        let many = RepoSnapshot {
            nodes: (0..12)
                .map(|i| change(&format!("node{i:02}"), NodeKind::Mutable, &[], &[]))
                .collect(),
            workstreams: vec![],
            bookmarks: vec![],
            ..stack()
        };
        let jumps: Vec<String> = listed(&context(Some(&many), None), "node")
            .into_iter()
            .filter(|id| id.starts_with("goto."))
            .collect();
        assert_eq!(jumps.len(), 8);
        assert_eq!(jumps[0], "goto.node00");
    }
}

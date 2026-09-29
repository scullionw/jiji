use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use jiji_forge::{LandOptions, MergeMethod};

#[derive(Debug, Parser)]
#[command(
    name = "jiji",
    bin_name = "jiji",
    version,
    about = "Manage local changes and publish, review, and land Jujutsu PR stacks",
    after_help = "HEAD defaults to the current stack. Use --dry-run to inspect a write command.\nAuthentication uses the same keychain, environment, or gh CLI token as the app.\nWith no command, or with a bare path, opens the desktop app."
)]
pub struct Cli {
    /// Repository directory (also accepts a nested directory or file)
    #[arg(short = 'R', long, global = true, default_value = ".")]
    pub repo: PathBuf,
    /// Emit JSON; watch emits one JSON event per line
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Put a change aside, out of ordinary jj and Git history
    #[command(
        after_help = "Operation history and reflogs still permit explicit recovery. After jj util gc,\nrepeat hide with the saved ID to remove any regenerated Git retention refs."
    )]
    Hide {
        /// Change ID, bookmark, or revset (defaults to the working copy)
        #[arg(default_value = "@")]
        revision: String,
        /// Hide the revision's mutable ancestors through the selected head
        #[arg(long)]
        stack: bool,
        /// Inspect the selection without recording edits or changing the repository
        #[arg(long)]
        dry_run: bool,
    },
    /// Restore a hidden change or stack using any saved change ID prefix
    Unhide {
        /// Saved change or commit ID (unambiguous prefixes are accepted)
        id: String,
        /// Preview recovery without changing the repository
        #[arg(long)]
        dry_run: bool,
    },
    /// List changes and stacks put aside with jiji hide
    Hidden,
    /// List published stacks, including stacks made with jjpr
    Stacks {
        /// Include local, unpublished work (shown automatically if none are published)
        #[arg(long)]
        all: bool,
        /// Match a title, bookmark, or PR number
        #[arg(long)]
        search: Option<String>,
        /// Read local stacks without contacting GitHub
        #[arg(long)]
        offline: bool,
    },
    /// Show a stack in landing order and its current landing readiness
    #[command(visible_alias = "stack")]
    Status {
        head: Option<String>,
        /// Read local stack structure without contacting GitHub
        #[arg(long)]
        offline: bool,
    },
    /// Push a stack and create or update its pull requests
    #[command(visible_alias = "submit")]
    Publish(PublishArgs),
    /// Mark a stack's draft PRs ready for review; nothing is pushed
    Ready(WriteArgs),
    /// Convert a stack's open PRs back to drafts; nothing is pushed
    Draft(WriteArgs),
    /// Land one round: merge or hand off, then reconcile what merged
    Land(LandArgs),
    /// Auto-land a stack until finished; Ctrl-C stops at the next safe point
    #[command(visible_alias = "auto-land")]
    Watch(WatchArgs),
    /// Ship local changes directly to trunk without opening pull requests
    Ship(WriteArgs),
    /// Inspect saved CLI auto-land jobs
    Jobs {
        #[arg(long)]
        state_file: Option<PathBuf>,
    },
    /// Browse pull requests, fetch one locally, or rerun its failed checks
    Pr {
        #[command(subcommand)]
        command: PrCommand,
    },
    /// Inspect or manage the shared GitHub connection
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Open this repository in the desktop app
    Open { path: Option<PathBuf> },
}

#[derive(Debug, Args)]
pub struct WriteArgs {
    /// Stack head; defaults to the current stack
    pub head: Option<String>,
    /// Print the plan without changing local or remote state
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct PublishArgs {
    #[command(flatten)]
    pub write: WriteArgs,
    /// Open new PRs as drafts; existing PRs keep their state (see `jiji draft`)
    #[arg(long)]
    pub draft: bool,
}

#[derive(Debug, Args)]
pub struct LandArgs {
    #[command(flatten)]
    pub write: WriteArgs,
    #[command(flatten)]
    pub rules: Rules,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Method {
    Auto,
    Squash,
    Rebase,
    Merge,
}

#[derive(Debug, Default, Args)]
pub struct Rules {
    /// Override GitHub's automatically selected merge method
    #[arg(long, value_enum)]
    pub method: Option<Method>,
    /// Attempt direct merges past checks, reviews, and merge queues (requires permission)
    #[arg(long, conflicts_with = "no_bypass")]
    pub bypass: bool,
    /// Turn off a bypass saved in a resumed watch
    #[arg(long, conflicts_with = "bypass")]
    pub no_bypass: bool,
}

impl Rules {
    pub fn apply(&self, mut saved: LandOptions) -> LandOptions {
        if let Some(method) = self.method {
            saved.method = match method {
                Method::Auto => None,
                Method::Squash => Some(MergeMethod::Squash),
                Method::Rebase => Some(MergeMethod::Rebase),
                Method::Merge => Some(MergeMethod::Merge),
            };
        }
        if self.bypass {
            saved.bypass = true;
        }
        if self.no_bypass {
            saved.bypass = false;
        }
        saved
    }
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    #[command(flatten)]
    pub land: LandArgs,
    /// Resume the saved job for this stack, preserving its progress and merge rules
    #[arg(long)]
    pub resume: bool,
    /// Override the default .jj/jiji/cli-autoland.json record file
    #[arg(long)]
    pub state_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum PrCommand {
    /// List open pull requests
    List,
    /// Show a pull request, including closed or merged PRs
    Show {
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        number: u64,
    },
    /// Fetch a pull request into a local review bookmark
    Fetch {
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        number: u64,
        #[arg(long)]
        bookmark: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    /// Rerun failed GitHub Actions runs for this PR's current head
    Rerun {
        #[arg(value_parser = clap::value_parser!(u64).range(1..))]
        number: u64,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Show the token source and connected GitHub account
    Status,
    /// Validate a token from stdin and store it in the system keychain
    Login {
        #[arg(long, required = true)]
        with_token: bool,
    },
    /// Remove Jiji's keychain token (environment and gh tokens are managed externally)
    Logout,
}

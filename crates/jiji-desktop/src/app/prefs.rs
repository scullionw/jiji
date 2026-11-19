//! Per-device preferences and the desktop-shell concerns that ride on
//! them: the theme, registration, the `jiji` launcher, editor and terminal
//! choices, and app updates.

use std::time::Duration;

use gpui::{AppContext as _, Context, PromptLevel, Task, WindowAppearance};

use super::AppModel;
use crate::host::cli::CliStatus;
use crate::host::integrations::{self, AppInfo};
use crate::host::license::{self, Refresh};
use crate::host::update::{self, InstallError, Staged};
use crate::host::HostResult;
use crate::store::{LicenseRecord, Store, ThemePref, Unreadable};
use crate::theme::{self, Scheme, Theme, DEFAULT_LIGHT};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

impl ThemeMode {
    fn parse(text: &str) -> Self {
        match text {
            "light" => ThemeMode::Light,
            "dark" => ThemeMode::Dark,
            _ => ThemeMode::System,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }
}

/// Registration state. The app and all core jj functionality are free; a
/// one-time license unlocks the theme system.
#[derive(Default)]
pub struct LicenseUi {
    pub registered: bool,
    /// "solo", "personal" or "unknown".
    pub plan: String,
    pub display_key: String,
    record: Option<LicenseRecord>,
    /// A simulated state (debug builds): never touches the store or Polar.
    simulated: bool,
    /// The licence file could not be read: `registered` is what it last
    /// said, until a read gets through.
    unread: bool,
}

/// Debug builds are not stuck behind the license gate (which pins the app
/// to the light theme). `JIJI_DEV_LICENSE=unregistered` exercises the gate
/// and `=real` uses the stored license; release builds always do the
/// latter.
fn dev_mode() -> Option<bool> {
    if !cfg!(debug_assertions) {
        return None;
    }
    match std::env::var("JIJI_DEV_LICENSE").as_deref() {
        Ok("unregistered") => Some(false),
        Ok("real") => None,
        _ => Some(true),
    }
}

impl LicenseUi {
    pub fn load(store: &mut Store) -> Self {
        match dev_mode() {
            Some(true) => Self {
                registered: true,
                plan: "personal".into(),
                display_key: "dev-mode".into(),
                simulated: true,
                ..Self::default()
            },
            Some(false) => Self {
                simulated: true,
                plan: "unknown".into(),
                ..Self::default()
            },
            None => {
                let mut this = Self {
                    plan: "unknown".into(),
                    ..Self::default()
                };
                this.read(store);
                this
            }
        }
    }

    /// Take in what the licence file says; false while it cannot be read.
    /// A file that cannot be read right now is not taken for an
    /// unregistered copy — a registered user keeps the themes through it.
    fn read(&mut self, store: &mut Store) -> bool {
        match store.license() {
            Ok(Some(record)) => self.adopt(record),
            Ok(None) => self.drop_record(),
            Err(Unreadable) => {
                self.registered = store.was_registered();
                self.unread = true;
                return false;
            }
        }
        self.unread = false;
        true
    }

    fn adopt(&mut self, record: LicenseRecord) {
        self.registered = true;
        self.plan = record.plan.clone();
        self.display_key = license::mask_key(&record.key);
        self.record = Some(record);
    }

    fn drop_record(&mut self) {
        self.registered = false;
        self.plan = "unknown".into();
        self.display_key.clear();
        self.record = None;
    }
}

/// The `jiji` terminal launcher: install state and the welcome-screen
/// offer.
pub struct CliUi {
    /// `None` until loaded — and forever on a platform without a launcher,
    /// which keeps every affordance off screen.
    pub status: Option<CliStatus>,
    /// Declining the offer is a durable choice; the palette stays the
    /// route back in.
    pub dismissed: bool,
    pub installing: bool,
    /// Keeps the welcome card up after a successful install to confirm
    /// where the command landed.
    pub just_installed: bool,
}

impl CliUi {
    pub fn new(store: &Store) -> Self {
        Self {
            status: None,
            dismissed: store.flag("cli.offer-dismissed"),
            installing: false,
            just_installed: false,
        }
    }
}

/// App updates: the downloaded one waiting for a restart, if any.
#[derive(Default)]
pub struct UpdateUi {
    pub version: Option<String>,
    pub installing: bool,
    staged: Option<Staged>,
    checking: bool,
}

/// Windows stay open for days; look again now and then.
const UPDATE_RECHECK: Duration = Duration::from_secs(6 * 60 * 60);

/// How soon a licence file that could not be read is tried again.
const LICENSE_RECHECK: Duration = Duration::from_secs(30);

impl AppModel {
    // ── Theme ──

    pub fn theme_pref(&self) -> ThemePref {
        self.store.theme()
    }

    pub fn theme_mode(&self) -> ThemeMode {
        ThemeMode::parse(&self.store.theme().mode)
    }

    fn resolved_scheme(&self, cx: &gpui::App) -> Scheme {
        match self.theme_mode() {
            ThemeMode::Light => Scheme::Light,
            ThemeMode::Dark => Scheme::Dark,
            ThemeMode::System => match cx.window_appearance() {
                WindowAppearance::Light | WindowAppearance::VibrantLight => Scheme::Light,
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Scheme::Dark,
            },
        }
    }

    /// Put the chosen palette on screen. Unregistered copies are pinned to
    /// the default light palette; registered users get their full
    /// preference, where following the OS switches between *their* dark
    /// and *their* light theme.
    pub fn apply_theme(&mut self, cx: &mut Context<Self>) {
        let pref = self.store.theme();
        let id = if !self.license.registered {
            DEFAULT_LIGHT.to_owned()
        } else if self.resolved_scheme(cx) == Scheme::Light {
            pref.light
        } else {
            pref.dark
        };
        if theme::has_theme(cx) && theme::theme(cx).id == id {
            return;
        }
        theme::fade_to(Theme::named(&id, self.mono_font.clone()), cx);
        cx.notify();
    }

    pub fn set_theme_mode(&mut self, mode: ThemeMode, cx: &mut Context<Self>) {
        if !self.license.registered {
            return;
        }
        let mut pref = self.store.theme();
        pref.mode = mode.as_str().into();
        self.store.set_theme(&pref);
        self.apply_theme(cx);
        cx.notify();
    }

    pub fn select_theme(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.license.registered {
            return;
        }
        let Some(scheme) = theme::scheme_of(id) else {
            return;
        };
        let mut pref = self.store.theme();
        match scheme {
            Scheme::Dark => pref.dark = id.to_owned(),
            Scheme::Light => pref.light = id.to_owned(),
        }
        // Picking a palette you can't currently see would be a dead click;
        // follow it with the mode when the schemes differ.
        if self.resolved_scheme(cx) != scheme {
            pref.mode = match scheme {
                Scheme::Dark => "dark".into(),
                Scheme::Light => "light".into(),
            };
        }
        self.store.set_theme(&pref);
        self.apply_theme(cx);
        cx.notify();
    }

    // ── Registration ──

    pub fn open_registration(&mut self, cx: &mut Context<Self>) {
        self.registration_open = true;
        cx.notify();
    }

    pub fn close_registration(&mut self, cx: &mut Context<Self>) {
        self.registration_open = false;
        cx.notify();
    }

    /// While the licence file cannot be read, try it again now and then:
    /// the registration is whole again as soon as it can be.
    pub(super) fn reread_license(&mut self, cx: &mut Context<Self>) {
        if !self.license.unread {
            return;
        }
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(LICENSE_RECHECK).await;
            let unread = this.update(cx, |this, cx| {
                this.recheck_license(cx);
                this.license.unread
            });
            if !unread.unwrap_or(false) {
                break;
            }
        })
        .detach();
    }

    fn recheck_license(&mut self, cx: &mut Context<Self>) {
        if self.license.unread && self.license.read(&mut self.store) {
            self.apply_theme(cx);
            self.revalidate_license(cx);
            cx.notify();
        }
    }

    /// Revalidate the cached license opportunistically (at most once a
    /// day). Offline keeps the cache; an explicit negative drops it.
    pub(super) fn revalidate_license(&mut self, cx: &mut Context<Self>) {
        if self.license.simulated {
            return;
        }
        let Some(record) = self.license.record.clone() else {
            return;
        };
        if !license::needs_refresh(&record) {
            return;
        }
        let refreshed =
            cx.background_spawn(async move { license::refresh(&license::PolarApi, &record) });
        cx.spawn(async move |this, cx| {
            let outcome = refreshed.await;
            let _ = this.update(cx, |this, cx| {
                match outcome {
                    Refresh::Keep(record) => {
                        this.store.set_license(&record);
                        this.license.adopt(record);
                    }
                    Refresh::Drop => {
                        this.store.clear_license();
                        this.license.drop_record();
                    }
                    Refresh::Unknown => {}
                }
                this.apply_theme(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Activate a freshly entered key. The error is the message to show.
    pub fn activate_license(
        &mut self,
        key: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        if self.license.simulated {
            return Task::ready(Err(
                "This is a development build with a simulated license.".into()
            ));
        }
        self.recheck_license(cx);
        let Ok(device_id) = self.store.device_id() else {
            return Task::ready(Err(
                "Jiji couldn’t read its licence file, so nothing was changed. Try again in a \
                 moment."
                    .into(),
            ));
        };
        let cached = self.license.record.clone();
        let activated = cx.background_spawn(async move {
            license::activate_license(&license::PolarApi, &key, &device_id, cached.as_ref())
        });
        cx.spawn(async move |this, cx| {
            let record = activated.await?;
            let _ = this.update(cx, |this, cx| {
                this.store.set_license(&record);
                this.license.adopt(record);
                this.apply_theme(cx);
                cx.notify();
            });
            Ok(())
        })
    }

    // ── The `jiji` launcher ──

    pub(super) fn sync_cli_status(&mut self, cx: &mut Context<Self>) {
        let status = cx.background_spawn(async move { crate::host::cli::cli_status().ok() });
        cx.spawn(async move |this, cx| {
            let status = status.await;
            let _ = this.update(cx, |this, cx| {
                this.cli.status = status;
                cx.notify();
            });
        })
        .detach();
    }

    /// Install (or refresh) the launcher. Declining the admin prompt is a
    /// decision, not a failure.
    pub fn install_cli(&mut self, cx: &mut Context<Self>) -> Task<HostResult<()>> {
        self.cli.installing = true;
        cx.notify();
        let installed = cx.background_spawn(async move { crate::host::cli::install_cli() });
        cx.spawn(async move |this, cx| {
            let result = installed.await;
            let _ = this.update(cx, |this, cx| {
                this.cli.installing = false;
                if let Ok(status) = &result {
                    this.cli.status = Some(status.clone());
                    this.cli.just_installed = true;
                }
                cx.notify();
            });
            match result {
                Ok(_) => Ok(()),
                Err(error) if error.code == "install_canceled" => Ok(()),
                Err(error) => Err(error),
            }
        })
    }

    pub fn uninstall_cli(&mut self, cx: &mut Context<Self>) -> Task<HostResult<()>> {
        let removed = cx.background_spawn(async move { crate::host::cli::uninstall_cli() });
        cx.spawn(async move |this, cx| {
            let status = removed.await?;
            let _ = this.update(cx, |this, cx| {
                this.cli.status = Some(status);
                this.cli.just_installed = false;
                cx.notify();
            });
            Ok(())
        })
    }

    pub fn dismiss_cli_offer(&mut self, cx: &mut Context<Self>) {
        self.cli.dismissed = true;
        self.store.set_pref("cli.offer-dismissed", &true);
        cx.notify();
    }

    // ── Editors and terminals ──

    pub(super) fn sync_integrations(&mut self, cx: &mut Context<Self>) {
        let detected = cx.background_spawn(async move {
            (
                integrations::detect_editors(),
                integrations::detect_terminals(),
            )
        });
        cx.spawn(async move |this, cx| {
            let (editors, terminals) = detected.await;
            let _ = this.update(cx, |this, cx| {
                this.editors = editors;
                this.terminals = terminals;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn preferred_editor(&self) -> Option<String> {
        self.store.pref("editor.preferred")
    }

    pub fn preferred_terminal(&self) -> Option<String> {
        self.store.pref("terminal.preferred")
    }

    /// Detected editors in the order menus offer them — the pinned
    /// favorite first when set, else the last-used one.
    pub fn editor_choices(&self) -> Vec<AppInfo> {
        let preferred = self.preferred_editor();
        let last: Option<String> = self.store.pref("editor.last");
        let find = |id: &Option<String>| {
            id.as_deref()
                .and_then(|id| self.editors.iter().find(|editor| editor.id == id))
        };
        let Some(first) = find(&preferred).or_else(|| find(&last)).cloned() else {
            return self.editors.clone();
        };
        let mut ordered = vec![first.clone()];
        ordered.extend(self.editors.iter().filter(|e| e.id != first.id).cloned());
        ordered
    }

    pub fn set_preferred_editor(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        match id {
            Some(id) => self.store.set_pref("editor.preferred", &id),
            None => self.store.clear_pref("editor.preferred"),
        }
        cx.notify();
    }

    pub fn set_preferred_terminal(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        match id {
            Some(id) => self.store.set_pref("terminal.preferred", &id),
            None => self.store.clear_pref("terminal.preferred"),
        }
        cx.notify();
    }

    pub fn open_in_editor(
        &mut self,
        editor: &str,
        path: String,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        self.store.set_pref("editor.last", &editor);
        if let Err(message) = integrations::open_in_editor(editor, &path, line) {
            self.set_error(message, cx);
        }
    }

    /// Open a directory in the pinned terminal while it is still
    /// installed, else the platform default.
    pub fn open_in_terminal(&mut self, path: &str, cx: &mut Context<Self>) {
        let preferred = self.preferred_terminal();
        let choice = self
            .terminals
            .iter()
            .find(|terminal| Some(terminal.id) == preferred.as_deref())
            .map(|terminal| terminal.id);
        if let Err(message) = integrations::open_in_terminal(path, choice) {
            self.set_error(message, cx);
        }
    }

    // ── App updates ──

    /// Checks at startup and every few hours, in builds that update
    /// themselves (see `update::supported`).
    pub(super) fn start_update_checks(&mut self, cx: &mut Context<Self>) {
        if !update::supported() {
            return;
        }
        cx.spawn(async move |this, cx| loop {
            let Ok(check) = this.update(cx, |this, cx| this.check_update_quietly(cx)) else {
                break;
            };
            check.await;
            cx.background_executor().timer(UPDATE_RECHECK).await;
        })
        .detach();
    }

    fn check_update_quietly(&mut self, cx: &mut Context<Self>) -> Task<()> {
        if self.update.staged.is_some() || self.update.checking {
            return Task::ready(());
        }
        self.update.checking = true;
        let dir = self.store.dir().to_path_buf();
        let staged = cx.background_spawn(async move {
            match update::check() {
                Ok(Some(found)) => update::download(&found, &dir).map(Some),
                Ok(None) => Ok(None),
                Err(err) => Err(err),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = staged.await;
            let _ = this.update(cx, |this, cx| {
                this.update.checking = false;
                match result {
                    Ok(Some(staged)) => {
                        this.update.version = Some(staged.version.clone());
                        this.update.staged = Some(staged);
                        cx.notify();
                    }
                    Ok(None) => {}
                    Err(err) => log::warn!("app update check failed: {err}"),
                }
            });
        })
    }

    /// Install the downloaded update and relaunch into it.
    pub fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        let Some(staged) = self.update.staged.clone() else {
            return;
        };
        if self.update.installing {
            return;
        }
        self.update.installing = true;
        cx.notify();
        let installed = cx.background_spawn(async move { update::install_and_relaunch(&staged) });
        cx.spawn(async move |this, cx| {
            let result = installed.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => cx.quit(),
                Err(err) => {
                    this.update.installing = false;
                    if err == InstallError::Unverified {
                        // Its archive is gone; fetch the release afresh.
                        this.update.staged = None;
                        this.update.version = None;
                        this.check_update_quietly(cx).detach();
                    }
                    log::warn!("app update install failed: {err}");
                    this.tell(
                        PromptLevel::Warning,
                        "Update failed",
                        &format!("The update could not be installed: {err}"),
                        cx,
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// A one-button native message.
    fn tell(&mut self, level: PromptLevel, title: &str, detail: &str, cx: &mut Context<Self>) {
        if let Some(window) = self.window {
            let _ = cx.update_window(window, |_, window, cx| {
                drop(window.prompt(level, title, Some(detail), &["OK"], cx));
            });
        }
    }

    /// The menu bar's "Check for Updates…": every outcome gets told to the
    /// user's face — found (with a choice), already current, or failed.
    pub fn check_for_update_now(&mut self, cx: &mut Context<Self>) {
        let Some(window) = self.window else {
            return;
        };
        if !update::supported() {
            self.tell(
                PromptLevel::Info,
                "Check for Updates",
                "This build was not installed as an app bundle, so it cannot update itself.",
                cx,
            );
            return;
        }
        if let Some(staged) = &self.update.staged {
            let answer = cx.update_window(window, |_, window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    "Update ready",
                    Some(&format!(
                        "Jiji {} is downloaded. Restart now to update?",
                        staged.version
                    )),
                    &["Restart", "Later"],
                    cx,
                )
            });
            if let Ok(answer) = answer {
                cx.spawn(async move |this, cx| {
                    if answer.await == Ok(0) {
                        let _ = this.update(cx, |this, cx| this.restart_to_update(cx));
                    }
                })
                .detach();
            }
            return;
        }
        let dir = self.store.dir().to_path_buf();
        let checked = cx.background_spawn(async move { update::check() });
        cx.spawn(async move |this, cx| {
            let found = match checked.await {
                Ok(Some(found)) => found,
                Ok(None) => {
                    let _ = this.update(cx, |this, cx| {
                        this.tell(
                            PromptLevel::Info,
                            "You're up to date",
                            &format!("Jiji {} is the latest version.", update::CURRENT_VERSION),
                            cx,
                        )
                    });
                    return;
                }
                Err(err) => {
                    let _ = this.update(cx, |this, cx| {
                        this.tell(
                            PromptLevel::Warning,
                            "Check for Updates",
                            &format!("The update check failed: {err}"),
                            cx,
                        )
                    });
                    return;
                }
            };
            let answer = cx.update_window(window, |_, window, cx| {
                window.prompt(
                    PromptLevel::Info,
                    "Update available",
                    Some(&format!(
                        "Jiji {} is available — you have {}. Update now?",
                        found.version,
                        update::CURRENT_VERSION
                    )),
                    &["Update", "Later"],
                    cx,
                )
            });
            let install_now = match answer {
                Ok(answer) => answer.await == Ok(0),
                Err(_) => false,
            };
            // Either way it downloads: "Later" keeps it for the status
            // bar's restart offer.
            let staged = cx
                .background_spawn(async move { update::download(&found, &dir) })
                .await;
            let _ = this.update(cx, |this, cx| match staged {
                Ok(staged) => {
                    this.update.version = Some(staged.version.clone());
                    this.update.staged = Some(staged);
                    cx.notify();
                    if install_now {
                        this.restart_to_update(cx);
                    }
                }
                Err(err) => this.tell(
                    PromptLevel::Warning,
                    "Check for Updates",
                    &format!("The update could not be downloaded: {err}"),
                    cx,
                ),
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    use super::*;

    #[gpui::test]
    fn a_new_palette_crosses_over_from_the_one_on_screen(cx: &mut gpui::TestAppContext) {
        let dir = crate::testing::scratch("crossfade");
        let model = crate::testing::install(&dir, cx);
        let bg =
            |cx: &mut gpui::TestAppContext| gpui::Rgba::from(cx.update(|cx| theme::theme(cx).bg1));
        let light = bg(cx);
        let dark = gpui::Rgba::from(Theme::named(theme::DEFAULT_DARK, "Menlo".into()).bg1);
        let switch = |mode: ThemeMode, cx: &mut gpui::TestAppContext| {
            model.update(cx, |model, cx| model.set_theme_mode(mode, cx));
        };
        let after = |ms: u64, cx: &mut gpui::TestAppContext| {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(ms));
            cx.run_until_parked();
        };

        cx.update(|cx| cx.set_reduce_motion(false));
        switch(ThemeMode::Dark, cx);
        // Partway, every surface is between the two palettes.
        after(100, cx);
        let partway = bg(cx);
        assert!(
            dark.r < partway.r && partway.r < light.r,
            "{} < {} < {}",
            dark.r,
            partway.r,
            light.r
        );
        // Then exactly the new one, and nothing more is scheduled.
        after(150, cx);
        assert_eq!(bg(cx), dark);
        assert_eq!(cx.update(|cx| theme::theme(cx).id), theme::DEFAULT_DARK);

        // Under Reduce Motion the switch is instant.
        cx.update(|cx| cx.set_reduce_motion(true));
        switch(ThemeMode::Light, cx);
        assert_eq!(bg(cx), light);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn record() -> LicenseRecord {
        LicenseRecord {
            key: "JIJI-ABCD-1234".into(),
            activation_id: "activation-1".into(),
            device_id: "device-1".into(),
            status: "granted".into(),
            plan: "personal".into(),
            limit_activations: Some(3),
            expires_at: None,
            registered_at: 1,
            last_validated_at: 1,
        }
    }

    /// The licence state a launch on `dir` starts with.
    fn launch(dir: &Path) -> (Store, LicenseUi) {
        let mut store = Store::at(dir.to_path_buf());
        let mut ui = LicenseUi::default();
        ui.read(&mut store);
        (store, ui)
    }

    #[test]
    fn a_registration_holds_while_its_licence_file_cannot_be_read() {
        let dir = crate::testing::scratch("licence-unread");
        let file = dir.join("license.json");
        let (mut store, _) = launch(&dir);
        let device = store.device_id().unwrap();
        store.set_license(&record());
        let good = std::fs::read(&file).unwrap();

        // Read refused, or the bytes not JSON at all.
        for how in ["permission", "decode"] {
            match how {
                "permission" => {
                    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap()
                }
                _ => std::fs::write(&file, b"{\"state\": {\"key\"").unwrap(),
            }
            let (mut store, mut ui) = launch(&dir);
            assert!(ui.registered && ui.unread, "{how}");
            assert_eq!(ui.record, None, "{how}");
            // No other device id, and nothing written over the file.
            assert_eq!(store.device_id(), Err(Unreadable), "{how}");

            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            if how == "decode" {
                assert_ne!(std::fs::read(&file).unwrap(), good);
                std::fs::write(&file, &good).unwrap();
            }
            assert_eq!(std::fs::read(&file).unwrap(), good, "{how}");
            // Back to normal at the next read.
            assert!(ui.read(&mut store), "{how}");
            assert!(ui.registered && !ui.unread, "{how}");
            assert_eq!(ui.record, Some(record()), "{how}");
            assert_eq!(ui.display_key, "••••1234", "{how}");
            assert_eq!(store.device_id(), Ok(device.clone()), "{how}");
        }

        // A copy never registered stays unregistered through the same.
        store.clear_license();
        std::fs::write(&file, b"not json").unwrap();
        let (_, ui) = launch(&dir);
        assert!(!ui.registered && ui.unread);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_licence_written_before_the_flag_existed_sets_it_when_read() {
        let dir = crate::testing::scratch("licence-upgrade");
        // An install from before: the licence file alone.
        let state = serde_json::json!({ "state": record(), "deviceId": "device-1" });
        std::fs::write(dir.join("license.json"), state.to_string()).unwrap();
        let (_, ui) = launch(&dir);
        assert!(ui.registered);

        std::fs::write(dir.join("license.json"), b"").unwrap();
        let (_, ui) = launch(&dir);
        assert!(ui.registered && ui.unread);
        let _ = std::fs::remove_dir_all(dir);
    }
}

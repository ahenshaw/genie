//! Dialogs for working with a Genie server: signing in, settling conflicts,
//! changing your password, managing accounts (administrators), and putting
//! the first tree on an empty server.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use egui::{Align, Layout, RichText, Ui};
use elegance::{Accent, BadgeTone, Button, ButtonSize, Modal, Select, Switch, TextInput, Theme, Toast, glyphs};

use crate::remote::{self, Account, ChangeRow, Conflict, LaterEdit, Remote, ReportQuery, Role};
use crate::widgets::{muted, section_label};

/// What a dialog asks the app to do.
pub enum Request {
    /// Signed in: open this working copy.
    Connected(Remote),
    /// Sync again, settling conflicts like this.
    Resolve(HashMap<String, &'static str>),
    /// Put a tree on the server: a `.ged` or `.gdz`, or the tree that was
    /// open before signing in. `replace`: over the tree already there.
    Seed { path: PathBuf, replace: bool },
    /// Show this person.
    Select(String),
    /// Get the latest from the server (after a revert there).
    Sync,
}

#[derive(Default)]
pub struct RemoteUi {
    connect: Option<ConnectDialog>,
    conflicts: Option<ConflictDialog>,
    password: Option<PasswordDialog>,
    accounts: Option<AccountsDialog>,
    seed: Option<SeedDialog>,
    report: Option<ReportDialog>,
    /// The accounts, for the report's filter as well as the Accounts dialog.
    account_list: Vec<Account>,
}

#[derive(Clone, Copy, PartialEq)]
enum Span {
    Today,
    Week,
    Month,
    All,
    Custom,
}

struct ReportDialog {
    open: bool,
    span: Span,
    from: String,
    to: String,
    /// "" for everyone.
    username: String,
    rows: Vec<ChangeRow>,
    more: bool,
    loading: bool,
    confirm: Option<RevertPlan>,
}

/// A revert waiting to be confirmed.
struct RevertPlan {
    revision: i64,
    xref: Option<String>,
    what: String,
    /// The server said later edits are in the way.
    blocked: Option<(String, Vec<LaterEdit>)>,
}

impl ReportDialog {
    fn query(&self) -> ReportQuery {
        let (from, to) = match self.span {
            Span::Today => (Some(remote::utc_day(0)), None),
            Span::Week => (Some(remote::utc_day(6)), None),
            Span::Month => (Some(remote::utc_day(29)), None),
            Span::All => (None, None),
            Span::Custom => (Some(self.from.trim().to_string()).filter(|s| !s.is_empty()), Some(self.to.trim().to_string()).filter(|s| !s.is_empty())),
        };
        ReportQuery { from, to, username: Some(self.username.clone()).filter(|u| !u.is_empty()) }
    }
}

struct ConnectDialog {
    open: bool,
    /// In the browser: signing in to the page's own server, for a cookie;
    /// there's nothing to show until then, so it can't be closed.
    web: bool,
    server: String,
    username: String,
    password: String,
    /// Why it's being shown again (the sign-in ended).
    reason: Option<String>,
    error: Option<String>,
    waiting: Option<Receiver<Result<Remote, String>>>,
}

struct ConflictDialog {
    open: bool,
    conflicts: Vec<Conflict>,
    /// Per record: keep this computer's version.
    mine: HashMap<String, bool>,
}

struct PasswordDialog {
    open: bool,
    current: String,
    new: String,
    again: String,
    error: Option<String>,
}

struct AccountsDialog {
    open: bool,
    accounts: Vec<Account>,
    new_username: String,
    new_display: String,
    new_password: String,
    new_role: Role,
    reset: HashMap<i32, String>,
    error: Option<String>,
}

struct SeedDialog {
    open: bool,
    /// The server already has a tree, which this replaces.
    replace: bool,
    /// The tree that was open before signing in, offered first.
    previous: Option<PathBuf>,
}

impl RemoteUi {
    pub fn open_connect(&mut self, server: &str, username: &str, reason: Option<String>) {
        self.connect = Some(ConnectDialog {
            open: true,
            web: crate::platform::WEB,
            server: if crate::platform::WEB {
                crate::platform::origin()
            } else if server.is_empty() {
                remote::DEFAULT_SERVER.to_string()
            } else {
                server.to_string()
            },
            username: username.to_string(),
            password: String::new(),
            reason,
            error: None,
            waiting: None,
        });
    }

    pub fn show_conflicts(&mut self, conflicts: Vec<Conflict>) {
        let mine = conflicts.iter().map(|c| (c.xref.clone(), true)).collect();
        self.conflicts = Some(ConflictDialog { open: true, conflicts, mine });
    }

    pub fn open_password(&mut self) {
        self.password = Some(PasswordDialog { open: true, current: String::new(), new: String::new(), again: String::new(), error: None });
    }

    pub fn open_accounts(&mut self, remote: &mut Remote, ctx: &egui::Context) {
        self.accounts = Some(AccountsDialog {
            open: true,
            accounts: Vec::new(),
            new_username: String::new(),
            new_display: String::new(),
            new_password: String::new(),
            new_role: Role::Family,
            reset: HashMap::new(),
            error: None,
        });
        refresh_accounts(remote, ctx);
    }

    pub fn accounts_open(&self) -> bool {
        self.accounts.as_ref().is_some_and(|a| a.open)
    }

    pub fn set_accounts(&mut self, list: Vec<Account>) {
        if let Some(a) = self.accounts.as_mut() {
            a.accounts = list.clone();
        }
        self.account_list = list;
    }

    pub fn open_report(&mut self, remote: &mut Remote, ctx: &egui::Context) {
        let d = ReportDialog {
            open: true,
            span: Span::Week,
            from: remote::utc_day(6),
            to: remote::utc_day(0),
            username: String::new(),
            rows: Vec::new(),
            more: false,
            loading: true,
            confirm: None,
        };
        remote.load_report(ctx, &d.query(), None);
        remote.load_accounts(ctx);
        self.report = Some(d);
    }

    pub fn report_rows(&mut self, rows: Vec<ChangeRow>, append: bool, more: bool) {
        if let Some(d) = self.report.as_mut() {
            if !append {
                d.rows.clear();
            }
            d.rows.extend(rows);
            d.more = more;
            d.loading = false;
        }
    }

    pub fn revert_blocked(&mut self, message: String, later: Vec<LaterEdit>) {
        if let Some(plan) = self.report.as_mut().and_then(|d| d.confirm.as_mut()) {
            plan.blocked = Some((message, later));
        }
    }

    /// A revert went in: show the report again from the top.
    pub fn reverted(&mut self, remote: &mut Remote, ctx: &egui::Context) {
        if let Some(d) = self.report.as_mut() {
            d.confirm = None;
            d.loading = true;
            remote.load_report(ctx, &d.query(), None);
        }
    }

    pub fn offer_seed(&mut self, previous: Option<PathBuf>, replace: bool) {
        self.seed = Some(SeedDialog { open: true, replace, previous });
    }

    /// Draws whatever is open. `remote` is the current connection, if any.
    pub fn show(&mut self, ctx: &egui::Context, mut remote: Option<&mut Remote>, requests: &mut Vec<Request>) {
        self.connect_dialog(ctx, requests);
        if let Some(r) = remote.as_deref_mut() {
            self.conflict_dialog(ctx, r, requests);
            self.password_dialog(ctx, r);
            self.accounts_dialog(ctx, r);
            self.seed_dialog(ctx, r, requests);
            self.report_dialog(ctx, r, requests);
        }
    }

    fn report_dialog(&mut self, ctx: &egui::Context, remote: &mut Remote, requests: &mut Vec<Request>) {
        let accounts: Vec<(String, String)> = self.account_list.iter().map(|a| (a.username.clone(), if a.display_name.is_empty() { a.username.clone() } else { a.display_name.clone() })).collect();
        let Some(d) = self.report.as_mut() else { return };
        let p = Theme::current(ctx).palette;
        let mut reload = false;
        let mut more = false;
        let mut plan: Option<RevertPlan> = None;
        let mut select = None;
        Modal::new("edit_report", &mut d.open)
            .heading("Edit history")
            .subtitle(format!("Every change to the tree on {}, and who made it", remote.host()))
            .header_icon(glyphs::KEY.to_string())
            .max_width(860.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let before = (d.span, d.username.clone());
                    let mut i = [Span::Today, Span::Week, Span::Month, Span::All, Span::Custom].iter().position(|s| *s == d.span).unwrap_or(1);
                    ui.add(elegance::SegmentedControl::new(&mut i, ["Today", "Last 7 days", "Last 30 days", "All", "Dates…"]).size(elegance::SegmentedSize::Small).id_salt("rep_span"));
                    d.span = [Span::Today, Span::Week, Span::Month, Span::All, Span::Custom][i];
                    ui.add_space(8.0);
                    let mut options = vec![(String::new(), "Everyone".to_string())];
                    options.extend(accounts.iter().cloned());
                    ui.add(Select::new("rep_user", &mut d.username).options(options).width(170.0));
                    reload |= before != (d.span, d.username.clone()) && d.span != Span::Custom;
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new(format!("{}  Copy as CSV", glyphs::COPY)).outline().size(ButtonSize::Small).enabled(!d.rows.is_empty())).clicked() {
                            ui.ctx().copy_text(csv(&d.rows));
                            Toast::new(format!("Copied {} changes", d.rows.len())).description("Paste them into a spreadsheet.").show(ui.ctx());
                        }
                    });
                });
                if d.span == Span::Custom {
                    ui.horizontal(|ui| {
                        ui.add(TextInput::new(&mut d.from).label("From").hint("YYYY-MM-DD").desired_width(130.0).id_salt("rep_from"));
                        ui.add(TextInput::new(&mut d.to).label("To").hint("YYYY-MM-DD").desired_width(130.0).id_salt("rep_to"));
                        if ui.add(Button::new("Show").size(ButtonSize::Small)).clicked() {
                            reload = true;
                        }
                    });
                }
                ui.label(RichText::new("Dates and times are UTC.").size(11.5).color(p.text_faint));
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(440.0).auto_shrink([false, true]).show(ui, |ui| {
                    if d.loading && d.rows.is_empty() {
                        ui.spinner();
                    } else if d.rows.is_empty() {
                        muted(ui, "No changes in that span.");
                    }
                    let mut i = 0;
                    while i < d.rows.len() {
                        let rev = d.rows[i].revision;
                        let group: Vec<&ChangeRow> = d.rows[i..].iter().take_while(|r| r.revision == rev).collect();
                        i += group.len();
                        let first = group[0];
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            let who = first.user.as_deref().unwrap_or("someone");
                            let n = group.len();
                            ui.label(RichText::new(format!("{} · {who}", first.when())).strong().color(p.text));
                            ui.label(RichText::new(format!("{n} change{}", if n == 1 { "" } else { "s" })).size(12.0).color(p.text_faint));
                            if !first.note.is_empty() {
                                ui.label(RichText::new(&first.note).size(12.0).color(p.amber));
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(Button::new("Revert this save").outline().size(ButtonSize::Small)).on_hover_text("Put everything it changed back as it was before").clicked() {
                                    plan = Some(RevertPlan { revision: rev, xref: None, what: format!("{who}'s save of {} ({n} change{})", first.when(), if n == 1 { "" } else { "s" }), blocked: None });
                                }
                            });
                        });
                        for r in group {
                            ui.horizontal(|ui| {
                                ui.add_space(14.0);
                                let (verb, color) = match r.action.as_str() {
                                    "add" => ("Added", p.green),
                                    "delete" => ("Deleted", p.red),
                                    _ => ("Changed", p.text_muted),
                                };
                                ui.label(RichText::new(verb).size(12.5).color(color));
                                let name = if r.label.is_empty() { r.xref.clone() } else { r.label.clone() };
                                if r.tag == "INDI" && r.action != "delete" {
                                    let link = ui.add(egui::Label::new(RichText::new(&name).size(13.0).color(p.focus)).sense(egui::Sense::click()));
                                    if link.on_hover_text("Show this person").clicked() {
                                        select = Some(r.xref.clone());
                                    }
                                } else {
                                    ui.label(RichText::new(&name).size(13.0).color(p.text));
                                }
                                ui.label(RichText::new(format!("{} {}", kind_word(&r.tag), r.xref)).size(11.5).color(p.text_faint));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(Button::new("Revert").outline().size(ButtonSize::Small)).on_hover_text("Put just this back as it was before this save").clicked() {
                                        let what = match r.action.as_str() {
                                            "add" => "addition of",
                                            "delete" => "deletion of",
                                            _ => "change to",
                                        };
                                        plan = Some(RevertPlan { revision: rev, xref: Some(r.xref.clone()), what: format!("{}'s {what} {name}", r.user.as_deref().unwrap_or("Someone")), blocked: None });
                                    }
                                });
                            });
                        }
                        ui.separator();
                    }
                    if d.more && ui.add(Button::new("Load more").outline().size(ButtonSize::Small).enabled(!d.loading)).clicked() {
                        more = true;
                    }
                });
            });
        if let Some(pl) = plan {
            d.confirm = Some(pl);
        }
        if let Some(x) = select {
            requests.push(Request::Select(x));
            d.open = false;
        }
        if reload {
            d.loading = true;
            remote.load_report(ctx, &d.query(), None);
        } else if more {
            d.loading = true;
            let before = d.rows.last().map(|r| r.revision);
            remote.load_report(ctx, &d.query(), before);
        }

        // Confirming a revert.
        let mut go = None;
        let mut cancel = false;
        if let Some(c) = d.confirm.as_ref() {
            let mut open = true;
            Modal::new("revert_confirm", &mut open)
                .heading("Revert this?")
                .subtitle(c.what.clone())
                .header_icon(glyphs::TRIANGLE_ALERT.to_string())
                .header_accent(Accent::Amber)
                .max_width(480.0)
                .show(ctx, |ui| {
                    match &c.blocked {
                        None => {
                            muted(ui, "What it changed goes back to how it was just before. The revert is saved as a change of its own, so it can be reverted too.");
                        }
                        Some((message, later)) => {
                            ui.label(RichText::new(message).color(p.amber));
                            ui.add_space(4.0);
                            for l in later.iter().take(12) {
                                let at = l.at.get(..16).unwrap_or(&l.at).replace('T', " ");
                                ui.label(RichText::new(format!("{} · {} · {at} UTC", l.label, l.user.as_deref().unwrap_or("someone"))).size(12.5).color(p.text_muted));
                            }
                            if later.len() > 12 {
                                ui.label(RichText::new(format!("and {} more", later.len() - 12)).size(12.0).color(p.text_faint));
                            }
                        }
                    }
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let force = c.blocked.is_some();
                        let label = if force { "Revert anyway" } else { "Revert" };
                        if ui.add(Button::new(label).accent(Accent::Red)).clicked() {
                            go = Some(force);
                        }
                        if ui.add(Button::new("Cancel").outline()).clicked() {
                            cancel = true;
                        }
                    });
                });
            cancel |= !open;
        }
        if let Some(force) = go
            && let Some(c) = d.confirm.as_ref()
        {
            remote.revert(ctx, c.revision, c.xref.clone(), force);
        }
        if cancel {
            d.confirm = None;
        }
        if !d.open {
            self.report = None;
        }
    }

    fn connect_dialog(&mut self, ctx: &egui::Context, requests: &mut Vec<Request>) {
        let Some(d) = self.connect.as_mut() else { return };
        if let Some(rx) = &d.waiting
            && let Ok(result) = rx.try_recv()
        {
            d.waiting = None;
            match result {
                Ok(remote) => {
                    requests.push(Request::Connected(remote));
                    self.connect = None;
                    return;
                }
                Err(e) => d.error = Some(e),
            }
        }
        let waiting = d.waiting.is_some();
        let mut submit = false;
        let web = d.web;
        let (heading, subtitle) = if web {
            (format!("Sign in to {}", remote::host_of(&d.server)), "The family tree you share".to_string())
        } else {
            ("Connect to a shared tree".to_string(), "Sign in to a Genie server to work on the family tree you share".to_string())
        };
        Modal::new("connect", &mut d.open)
            .heading(heading)
            .subtitle(subtitle)
            .header_icon(glyphs::HOME.to_string())
            .closable(!web)
            .close_on_backdrop(!web)
            .close_on_escape(!web)
            .max_width(440.0)
            .show(ctx, |ui| {
                if let Some(r) = &d.reason {
                    ui.label(RichText::new(r).color(Theme::current(ui.ctx()).palette.amber));
                    ui.add_space(6.0);
                }
                if !web {
                    ui.add(TextInput::new(&mut d.server).label("Server").hint("https://genie.henshaw.us").id_salt("c_server"));
                }
                ui.add(TextInput::new(&mut d.username).label("Username").id_salt("c_user"));
                let pw = ui.add(TextInput::new(&mut d.password).label("Password").password(true).id_salt("c_pass"));
                if pw.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if let Some(e) = &d.error {
                    ui.add_space(4.0);
                    ui.label(RichText::new(e).color(Theme::current(ui.ctx()).palette.red));
                }
                ui.add_space(8.0);
                if !web {
                    muted(ui, "Your copy of the tree is kept on this computer too, so you can keep working without a connection and sync later.");
                }
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let ok = !waiting && !d.username.trim().is_empty() && !d.password.is_empty();
                    if ui.add(Button::new(if waiting { "Signing in…" } else { "Sign in" }).accent(Accent::Green).enabled(ok)).clicked() {
                        submit = true;
                    }
                    if waiting {
                        ui.spinner();
                    }
                });
            });
        if submit && !waiting && !d.username.trim().is_empty() {
            if web {
                d.waiting = Some(remote::sign_in_web(ctx, &d.server, &d.username, &d.password));
            } else {
                let (tx, rx) = channel();
                let (server, user, pass, ctx) = (d.server.clone(), d.username.clone(), d.password.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(remote::sign_in(&server, &user, &pass));
                    ctx.request_repaint();
                });
                d.waiting = Some(rx);
            }
            d.error = None;
        }
        if !d.open && !waiting {
            self.connect = None;
        }
    }

    fn conflict_dialog(&mut self, ctx: &egui::Context, remote: &Remote, requests: &mut Vec<Request>) {
        let Some(d) = self.conflicts.as_mut() else { return };
        let p = Theme::current(ctx).palette;
        let mut go = false;
        Modal::new("conflicts", &mut d.open)
            .heading("Someone else changed the same people")
            .subtitle(format!("Choose whose version to keep, then sync again with {}", remote.host()))
            .header_icon(glyphs::TRIANGLE_ALERT.to_string())
            .header_accent(Accent::Amber)
            .max_width(560.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for c in &d.conflicts {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(300.0);
                                let name = if c.label.is_empty() { c.xref.clone() } else { c.label.clone() };
                                ui.label(RichText::new(name).strong().color(p.text));
                                ui.label(RichText::new(c.describe()).size(12.0).color(p.text_muted));
                            });
                            let mine = d.mine.entry(c.xref.clone()).or_insert(true);
                            let mut which = if *mine { 0 } else { 1 };
                            ui.add(elegance::SegmentedControl::new(&mut which, ["Keep mine", "Keep theirs"]).size(elegance::SegmentedSize::Small).id_salt(("cf", &c.xref)));
                            *mine = which == 0;
                        });
                        ui.separator();
                    }
                });
                ui.add_space(6.0);
                muted(ui, "Everything else is merged either way. Keeping theirs discards your change to that person.");
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new(format!("{}  Sync with these choices", glyphs::CHECK)).accent(Accent::Green)).clicked() {
                        go = true;
                    }
                });
            });
        if go {
            let resolve = d.mine.iter().map(|(x, mine)| (x.clone(), if *mine { "mine" } else { "theirs" })).collect();
            requests.push(Request::Resolve(resolve));
            self.conflicts = None;
        } else if !d.open {
            self.conflicts = None;
        }
    }

    fn password_dialog(&mut self, ctx: &egui::Context, remote: &mut Remote) {
        let Some(d) = self.password.as_mut() else { return };
        let mut go = false;
        Modal::new("password", &mut d.open).heading("Change your password").subtitle(format!("For {} on {}", remote.state.user.username, remote.host())).max_width(420.0).show(ctx, |ui| {
            ui.add(TextInput::new(&mut d.current).label("Current password").password(true).id_salt("pw_cur"));
            ui.add(TextInput::new(&mut d.new).label("New password").hint("At least 10 characters").password(true).id_salt("pw_new"));
            ui.add(TextInput::new(&mut d.again).label("New password again").password(true).id_salt("pw_again"));
            if let Some(e) = &d.error {
                ui.label(RichText::new(e).color(Theme::current(ui.ctx()).palette.red));
            }
            muted(ui, "You'll stay signed in here; other computers will need the new password.");
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new("Change password").accent(Accent::Green).enabled(!d.current.is_empty() && !d.new.is_empty())).clicked() {
                    go = true;
                }
            });
        });
        if go {
            if d.new != d.again {
                d.error = Some("The new passwords don't match.".into());
            } else if d.new.chars().count() < 10 {
                d.error = Some("Use at least 10 characters.".into());
            } else {
                remote.change_password(ctx, d.current.clone(), d.new.clone());
                self.password = None;
                return;
            }
        }
        if !d.open {
            self.password = None;
        }
    }

    fn accounts_dialog(&mut self, ctx: &egui::Context, remote: &mut Remote) {
        let Some(d) = self.accounts.as_mut() else { return };
        let p = Theme::current(ctx).palette;
        let me = remote.state.user.id;
        let mut changes: Vec<(i32, serde_json::Value, &'static str)> = Vec::new();
        let mut create = false;
        Modal::new("accounts", &mut d.open)
            .heading("Accounts")
            .subtitle(format!("Who can see and edit the tree on {}", remote.host()))
            .header_icon(glyphs::KEY.to_string())
            .max_width(720.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(340.0).show(ui, |ui| {
                    if d.accounts.is_empty() {
                        ui.spinner();
                    }
                    for a in &d.accounts {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(200.0);
                                let name = if a.display_name.is_empty() { a.username.clone() } else { format!("{} ({})", a.display_name, a.username) };
                                ui.label(RichText::new(name).strong().color(if a.disabled { p.text_faint } else { p.text }));
                                let seen = a.last_login_at.as_deref().map(|t| format!("Last signed in {}", t.get(..10).unwrap_or(t))).unwrap_or_else(|| "Never signed in".into());
                                ui.label(RichText::new(seen).size(11.5).color(p.text_faint));
                            });
                            let mut role = Role::parse(&a.role).unwrap_or(Role::Guest);
                            let before = role;
                            ui.add(Select::new(("acct_role", a.id), &mut role).options(Role::ALL.map(|r| (r, r.label()))).width(140.0));
                            if role != before {
                                changes.push((a.id, serde_json::json!({ "role": role }), "Role changed; they'll need to sign in again."));
                            }
                            let mut enabled = !a.disabled;
                            if ui.add(Switch::new(&mut enabled, "Active").enabled(a.id != me)).changed() {
                                changes.push((a.id, serde_json::json!({ "disabled": !enabled }), if enabled { "Account enabled." } else { "Account disabled." }));
                            }
                            let pw = d.reset.entry(a.id).or_default();
                            ui.add(TextInput::new(pw).hint("New password").password(true).desired_width(130.0).compact(true).id_salt(("acct_pw", a.id)));
                            if ui.add(Button::new("Set").size(ButtonSize::Small).outline().enabled(pw.chars().count() >= 10)).on_hover_text("Set this password; they'll need it to sign in").clicked() {
                                changes.push((a.id, serde_json::json!({ "password": pw.clone() }), "Password set."));
                                pw.clear();
                            }
                        });
                        ui.separator();
                    }
                });
                ui.add_space(6.0);
                section_label(ui, "Add someone");
                ui.horizontal(|ui| {
                    ui.add(TextInput::new(&mut d.new_username).hint("username").desired_width(120.0).compact(true).id_salt("na_user"));
                    ui.add(TextInput::new(&mut d.new_display).hint("Name shown on changes").desired_width(170.0).compact(true).id_salt("na_name"));
                    ui.add(TextInput::new(&mut d.new_password).hint("Password (10+)").password(true).desired_width(130.0).compact(true).id_salt("na_pw"));
                    ui.add(Select::new("na_role", &mut d.new_role).options(Role::ALL.map(|r| (r, r.label()))).width(130.0));
                    let ok = d.new_username.trim().len() >= 2 && d.new_password.chars().count() >= 10;
                    if ui.add(Button::new(format!("{}  Add", glyphs::PLUS)).size(ButtonSize::Small).accent(Accent::Green).enabled(ok)).clicked() {
                        create = true;
                    }
                });
                ui.label(RichText::new(format!("{}: {}", d.new_role.label(), d.new_role.describe())).size(11.5).color(p.text_faint));
                if let Some(e) = &d.error {
                    ui.label(RichText::new(e).color(p.red));
                }
            });
        for (id, change, done) in changes {
            remote.update_account(ctx, id, change, done.into());
        }
        if create {
            let (u, n, pw, role) = (d.new_username.trim().to_string(), d.new_display.trim().to_string(), d.new_password.clone(), d.new_role);
            d.new_username.clear();
            d.new_display.clear();
            d.new_password.clear();
            remote.create_account(ctx, u, n, pw, role);
        }
        if !d.open {
            self.accounts = None;
        }
    }

    fn seed_dialog(&mut self, ctx: &egui::Context, remote: &Remote, requests: &mut Vec<Request>) {
        let Some(d) = self.seed.as_mut() else { return };
        let mut chosen = None;
        let (heading, subtitle) = if d.replace {
            ("Replace the shared tree", format!("Everyone on {} will get the tree you upload", remote.host()))
        } else {
            ("The shared tree is empty", format!("Put a tree on {} for everyone to work on", remote.host()))
        };
        let replace = d.replace;
        Modal::new("seed", &mut d.open)
            .heading(heading)
            .subtitle(subtitle)
            .header_icon(glyphs::DOWNLOAD.to_string())
            .header_accent(if replace { Accent::Amber } else { Accent::Blue })
            .max_width(480.0)
            .show(ctx, |ui| {
                if replace {
                    muted(ui, "The tree there now is replaced, and kept in its history. Changes others haven't synced yet will conflict with it.");
                    ui.add_space(6.0);
                }
                muted(ui, "Upload a tree with its documents. A .gdz bundle brings its photos and documents; a .ged file brings the documents it links to on this computer.");
                ui.add_space(10.0);
                if let Some(prev) = &d.previous {
                    let name = prev.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    if ui.add(Button::new(format!("{}  Upload {name}", glyphs::DOWNLOAD)).accent(Accent::Green)).on_hover_text(prev.display().to_string()).clicked() {
                        chosen = Some(prev.clone());
                    }
                    ui.add_space(4.0);
                }
                if ui.add(Button::new(format!("{}  Choose a file…", glyphs::FOLDER_OPEN)).outline()).clicked() {
                    chosen = crate::platform::pick_ged(None);
                }
            });
        if let Some(path) = chosen {
            requests.push(Request::Seed { path, replace });
            self.seed = None;
        } else if !d.open {
            self.seed = None;
        }
    }
}

fn kind_word(tag: &str) -> &'static str {
    match tag {
        "INDI" => "person",
        "FAM" => "family",
        "SOUR" => "source",
        "OBJE" => "document",
        "NOTE" => "note",
        "REPO" => "repository",
        _ => "record",
    }
}

/// The report as CSV, one change per line.
fn csv(rows: &[ChangeRow]) -> String {
    let field = |s: &str| if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() };
    let mut out = String::from("When (UTC),Who,Account,Action,What,Record,ID,Revision,Note\n");
    for r in rows {
        let when = r.at.replace('T', " ").trim_end_matches('Z').to_string();
        let line = [
            when,
            r.user.clone().unwrap_or_default(),
            r.username.clone().unwrap_or_default(),
            r.action.clone(),
            kind_word(&r.tag).to_string(),
            r.label.clone(),
            r.xref.clone(),
            r.revision.to_string(),
            r.note.clone(),
        ]
        .iter()
        .map(|f| field(f))
        .collect::<Vec<_>>()
        .join(",");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

pub fn refresh_accounts(remote: &mut Remote, ctx: &egui::Context) {
    remote.load_accounts(ctx);
}

/// "Syncing with genie.henshaw.us…", over everything while the tree is exchanged.
pub fn syncing_overlay(ctx: &egui::Context, host: &str) {
    let mut open = true;
    Modal::new("syncing", &mut open).closable(false).close_on_backdrop(false).close_on_escape(false).max_width(360.0).show(ctx, |ui: &mut Ui| {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(format!("Syncing with {host}…"));
        });
    });
}

/// A toast for an error from the server.
pub fn failed(ctx: &egui::Context, host: &str, message: &str) {
    Toast::new(format!("Couldn't sync with {host}")).tone(BadgeTone::Danger).description(message.to_string()).show(ctx);
}

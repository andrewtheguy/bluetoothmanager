//! Application state and input handling.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot;
use zbus::zvariant::OwnedObjectPath;

use crate::bluez::{
    Adapter, AgentReply, AgentRequest, AgentRequestKind, BluezClient, Device, Snapshot,
    describe_uuid,
};

// ---------------------------------------------------------------- app messages

#[derive(Debug)]
pub enum Msg {
    Snapshot(Box<Snapshot>),
    Status(Status),
    /// BlueZ needs an answer from the user to finish a pairing.
    Agent(AgentRequest),
    /// BlueZ wants the user to see something (a passkey to type on the device).
    AgentDisplay { device: OwnedObjectPath, text: String },
    /// BlueZ withdrew its request.
    AgentCancel,
    /// Our own pairing finished, one way or the other.
    AgentDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub level: Level,
    pub text: String,
    /// Set while a long-running action is in flight, so the UI can spin.
    pub busy: bool,
}

impl Status {
    pub fn info(text: impl Into<String>) -> Self {
        Self { level: Level::Info, text: text.into(), busy: false }
    }
    pub fn busy(text: impl Into<String>) -> Self {
        Self { level: Level::Info, text: text.into(), busy: true }
    }
    pub fn ok(text: impl Into<String>) -> Self {
        Self { level: Level::Ok, text: text.into(), busy: false }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self { level: Level::Error, text: text.into(), busy: false }
    }
}

// -------------------------------------------------------------------- modals

#[derive(Debug)]
pub struct Field {
    pub label: &'static str,
    pub value: String,
    pub secret: bool,
}

#[derive(Debug)]
pub enum PromptKind {
    RenameDevice { device: OwnedObjectPath },
    RenameAdapter { adapter: OwnedObjectPath },
    /// Legacy pairing: the PIN printed on the device.
    AgentPin(oneshot::Sender<AgentReply>),
    /// The six-digit passkey the device is showing.
    AgentPasskey(oneshot::Sender<AgentReply>),
}

#[derive(Debug)]
pub struct Prompt {
    pub title: String,
    pub hint: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub reveal: bool,
    pub kind: PromptKind,
}

impl Prompt {
    pub fn submit_label(&self) -> &'static str {
        match self.kind {
            PromptKind::RenameDevice { .. } | PromptKind::RenameAdapter { .. } => "rename",
            PromptKind::AgentPin(_) | PromptKind::AgentPasskey(_) => "pair",
        }
    }
}

#[derive(Debug)]
pub enum ConfirmKind {
    Forget {
        name: String,
        adapter: OwnedObjectPath,
        device: OwnedObjectPath,
    },
    /// A yes/no BlueZ is waiting on: passkey match, incoming pairing, or a
    /// service authorisation.
    Agent {
        title: String,
        body: String,
        reply: oneshot::Sender<AgentReply>,
    },
}

#[derive(Debug)]
pub enum Modal {
    None,
    Help,
    Prompt(Prompt),
    Confirm(ConfirmKind),
    /// Something to read, no answer needed: dismissed by any key.
    Info { title: String, text: String },
}

impl Modal {
    fn is_agent(&self) -> bool {
        self.holds_reply() || matches!(self, Modal::Info { .. })
    }

    /// BlueZ is waiting on this dialog: dropping it would drop the reply
    /// channel and cancel the pairing.
    fn holds_reply(&self) -> bool {
        match self {
            Modal::Prompt(p) => matches!(p.kind, PromptKind::AgentPin(_) | PromptKind::AgentPasskey(_)),
            Modal::Confirm(ConfirmKind::Agent { .. }) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Adapters,
    Devices,
}

// ----------------------------------------------------------------------- app

pub struct App {
    client: Arc<BluezClient>,
    tx: Sender<Msg>,
    poke: Sender<()>,

    pub snapshot: Snapshot,
    pub first_load: bool,
    pub focus: Focus,
    pub modal: Modal,
    pub status: Status,
    pub status_at: Instant,
    pub quit: bool,

    /// Selections are held as identities rather than indices so a refresh that
    /// reorders the lists does not move the cursor out from under the user.
    selected_adapter: Option<OwnedObjectPath>,
    selected_device: Option<OwnedObjectPath>,
}

impl App {
    pub fn new(client: Arc<BluezClient>, tx: Sender<Msg>, poke: Sender<()>) -> Self {
        Self {
            client,
            tx,
            poke,
            snapshot: Snapshot::default(),
            first_load: true,
            focus: Focus::Devices,
            modal: Modal::None,
            status: Status::info("connecting to BlueZ…"),
            status_at: Instant::now(),
            quit: false,
            selected_adapter: None,
            selected_device: None,
        }
    }

    // ---------------------------------------------------------- selection

    pub fn adapters(&self) -> &[Adapter] {
        &self.snapshot.adapters
    }

    pub fn adapter_index(&self) -> usize {
        self.selected_adapter
            .as_ref()
            .and_then(|p| self.adapters().iter().position(|a| &a.path == p))
            .unwrap_or(0)
    }

    pub fn adapter(&self) -> Option<&Adapter> {
        self.adapters().get(self.adapter_index())
    }

    pub fn devices(&self) -> &[Device] {
        self.adapter().map(|a| a.devices.as_slice()).unwrap_or(&[])
    }

    pub fn device_index(&self) -> usize {
        self.selected_device
            .as_ref()
            .and_then(|p| self.devices().iter().position(|d| &d.path == p))
            .unwrap_or(0)
    }

    pub fn device(&self) -> Option<&Device> {
        self.devices().get(self.device_index())
    }

    fn select_adapter(&mut self, idx: usize) {
        if let Some(a) = self.adapters().get(idx) {
            self.selected_adapter = Some(a.path.clone());
            self.selected_device = None;
        }
    }

    fn select_device(&mut self, idx: usize) {
        if let Some(d) = self.devices().get(idx) {
            self.selected_device = Some(d.path.clone());
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Adapters => {
                let len = self.adapters().len();
                if len == 0 {
                    return;
                }
                let idx = clamp_move(self.adapter_index(), delta, len);
                self.select_adapter(idx);
            }
            Focus::Devices => {
                let len = self.devices().len();
                if len == 0 {
                    return;
                }
                let idx = clamp_move(self.device_index(), delta, len);
                self.select_device(idx);
            }
        }
    }

    // ------------------------------------------------------------ messages

    pub fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Snapshot(s) => {
                let was_loading = self.first_load;
                self.snapshot = *s;
                self.first_load = false;
                if self.selected_adapter.is_none() {
                    self.select_adapter(0);
                }
                // Pin the device cursor to a specific device rather than leaving
                // it at "whatever is row 0", so a scan that reorders the list
                // cannot move it out from under the user mid-keystroke.
                let adrift = self
                    .selected_device
                    .as_ref()
                    .is_none_or(|p| !self.devices().iter().any(|d| &d.path == p));
                if adrift {
                    self.select_device(0);
                }
                // Retire the start-up placeholder once there is something to
                // look at; anything else on screen is the user's own business.
                if was_loading {
                    self.status = Status::info(String::new());
                }
            }
            Msg::Status(s) => {
                self.status = s;
                self.status_at = Instant::now();
            }
            Msg::Agent(req) => self.on_agent_request(req),
            Msg::AgentDisplay { device, text } => {
                // A dialog BlueZ is waiting on outranks a card it only wants seen.
                if !self.modal.holds_reply() {
                    let name = self.device_name(&device);
                    self.modal = Modal::Info { title: format!("Pairing with {name}"), text };
                }
            }
            Msg::AgentCancel => {
                if self.modal.is_agent() {
                    self.modal = Modal::None;
                }
            }
            Msg::AgentDone => {
                if matches!(self.modal, Modal::Info { .. }) {
                    self.modal = Modal::None;
                }
            }
        }
    }

    fn device_name(&self, path: &OwnedObjectPath) -> String {
        self.snapshot
            .find_device(path)
            .map(Device::display_name)
            .unwrap_or_else(|| {
                // BlueZ paths end in `dev_XX_XX_XX_XX_XX_XX`.
                path.as_str()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .trim_start_matches("dev_")
                    .replace('_', ":")
            })
    }

    /// Turn a BlueZ agent callback into the dialog that answers it. Whatever
    /// was open loses; a pairing prompt has a deadline and the user's own
    /// dialog does not.
    fn on_agent_request(&mut self, req: AgentRequest) {
        let name = self.device_name(&req.device);
        // Follow the device the prompt is about, so the detail pane explains
        // who is asking.
        if let Some(adapter) = self.adapters().iter().find(|a| a.devices.iter().any(|d| d.path == req.device)) {
            self.selected_adapter = Some(adapter.path.clone());
            self.selected_device = Some(req.device.clone());
        }
        self.modal = match req.kind {
            AgentRequestKind::PinCode => Modal::Prompt(Prompt {
                title: format!("Pair with {name}"),
                hint: "type the PIN printed on or shown by the device (often 0000 or 1234)".into(),
                fields: vec![Field { label: "PIN", value: String::new(), secret: false }],
                focus: 0,
                reveal: true,
                kind: PromptKind::AgentPin(req.reply),
            }),
            AgentRequestKind::Passkey => Modal::Prompt(Prompt {
                title: format!("Pair with {name}"),
                hint: "type the 6-digit passkey the device is showing".into(),
                fields: vec![Field { label: "Passkey", value: String::new(), secret: false }],
                focus: 0,
                reveal: true,
                kind: PromptKind::AgentPasskey(req.reply),
            }),
            AgentRequestKind::Confirm { passkey } => Modal::Confirm(ConfirmKind::Agent {
                title: "Confirm pairing".into(),
                body: format!("Does {name} show the passkey {passkey:06}?"),
                reply: req.reply,
            }),
            AgentRequestKind::Authorize => Modal::Confirm(ConfirmKind::Agent {
                title: "Pairing request".into(),
                body: format!("{name} wants to pair with this computer."),
                reply: req.reply,
            }),
            AgentRequestKind::AuthorizeService { uuid } => Modal::Confirm(ConfirmKind::Agent {
                title: "Connection request".into(),
                body: format!("{name} wants to use {} on this computer.", describe_uuid(&uuid)),
                reply: req.reply,
            }),
        };
    }

    pub fn status_visible(&self) -> bool {
        self.status.busy
            || self.status.level == Level::Error
            || self.status_at.elapsed() < Duration::from_secs(6)
    }

    fn set_status(&mut self, s: Status) {
        self.status = s;
        self.status_at = Instant::now();
    }

    /// Run a fallible action off the UI thread, reporting the outcome in the
    /// status bar and refreshing the view when it lands.
    fn spawn<F>(&self, pending: &str, done: String, fut: F)
    where
        F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let tx = self.tx.clone();
        let poke = self.poke.clone();
        let _ = tx.try_send(Msg::Status(Status::busy(pending)));
        tokio::spawn(async move {
            let status = match fut.await {
                Ok(()) => Status::ok(done),
                Err(e) => Status::error(format_error(&e)),
            };
            let _ = tx.send(Msg::Status(status)).await;
            let _ = poke.send(()).await;
        });
    }

    // -------------------------------------------------------------- input

    pub fn on_event(&mut self, ev: Event) {
        let Event::Key(key) = ev else { return };
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.modal, Modal::None) {
            Modal::None => self.on_key_normal(key),
            // Any key dismisses these overlays; taking them out of `self.modal`
            // above is the whole action.
            Modal::Help | Modal::Info { .. } => {}
            Modal::Prompt(p) => self.on_key_prompt(key, p),
            Modal::Confirm(c) => self.on_key_confirm(key, c),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.modal = Modal::Help,
            KeyCode::Esc => self.set_status(Status::info("")),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Adapters => Focus::Devices,
                    Focus::Devices => Focus::Adapters,
                }
            }
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Adapters,
            KeyCode::Right | KeyCode::Char('l') => self.focus = Focus::Devices,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::PageDown => self.move_selection(10),
            KeyCode::PageUp => self.move_selection(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(isize::MIN / 2),
            KeyCode::End | KeyCode::Char('G') => self.move_selection(isize::MAX / 2),
            KeyCode::Enter => self.connect_selected(),
            KeyCode::Char('d') => self.disconnect(),
            KeyCode::Char('s') | KeyCode::Char('r') => self.toggle_scan(),
            KeyCode::Char('w') => self.toggle_power(),
            KeyCode::Char('v') => self.toggle_discoverable(),
            KeyCode::Char('p') => self.toggle_pairable(),
            KeyCode::Char('t') => self.toggle_trusted(),
            KeyCode::Char('b') => self.toggle_blocked(),
            KeyCode::Char('f') => self.forget(),
            KeyCode::Char('n') => self.rename(),
            _ => {}
        }
    }

    fn on_key_prompt(&mut self, key: KeyEvent, mut p: Prompt) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                // Dropping an agent prompt drops its reply channel, which the
                // agent reports to BlueZ as a cancellation.
                return;
            }
            KeyCode::Enter => {
                self.submit_prompt(p);
                return;
            }
            KeyCode::Tab | KeyCode::Down => p.focus = (p.focus + 1) % p.fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                p.focus = (p.focus + p.fields.len() - 1) % p.fields.len()
            }
            KeyCode::Backspace => {
                p.fields[p.focus].value.pop();
            }
            KeyCode::Char('u') if ctrl => p.fields[p.focus].value.clear(),
            KeyCode::Char('r') if ctrl => p.reveal = !p.reveal,
            KeyCode::Char(c) if !ctrl => p.fields[p.focus].value.push(c),
            _ => {}
        }
        self.modal = Modal::Prompt(p);
    }

    fn on_key_confirm(&mut self, key: KeyEvent, c: ConfirmKind) {
        match c {
            ConfirmKind::Forget { name, adapter, device } => {
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    let client = self.client.clone();
                    self.spawn(
                        format!("forgetting {name}…").as_str(),
                        format!("forgot {name}"),
                        async move { client.remove_device(&adapter, &device).await },
                    );
                }
            }
            ConfirmKind::Agent { title, body, reply } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    let _ = reply.send(AgentReply::Accept);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    let _ = reply.send(AgentReply::Reject);
                }
                // BlueZ is still waiting; an unrelated key must not answer it.
                _ => self.modal = Modal::Confirm(ConfirmKind::Agent { title, body, reply }),
            },
        }
    }

    // ------------------------------------------------------------ actions

    fn toggle_power(&mut self) {
        let Some(a) = self.adapter() else {
            self.set_status(Status::error("no Bluetooth adapter"));
            return;
        };
        if a.blocked() {
            self.set_status(Status::error("Bluetooth is blocked by rfkill or a hardware switch"));
            return;
        }
        let (path, id, on) = (a.path.clone(), a.id.clone(), !a.powered);
        let client = self.client.clone();
        self.spawn(
            if on { "powering on…" } else { "powering off…" },
            format!("{id} powered {}", on_off(on)),
            async move { client.set_powered(&path, on).await },
        );
    }

    fn toggle_scan(&mut self) {
        let Some(a) = self.adapter() else {
            self.set_status(Status::error("no Bluetooth adapter"));
            return;
        };
        if !a.powered {
            self.set_status(Status::error("Bluetooth is off — press w to turn it on"));
            return;
        }
        let (path, id, stop) = (a.path.clone(), a.id.clone(), a.discovering);
        let client = self.client.clone();
        if stop {
            self.spawn(
                "stopping scan…",
                format!("scan stopped on {id}"),
                async move { client.stop_discovery(&path).await },
            );
        } else {
            self.spawn(
                format!("scanning on {id}…").as_str(),
                format!("scanning on {id} — press s again to stop"),
                async move { client.start_discovery(&path).await },
            );
        }
    }

    fn toggle_discoverable(&mut self) {
        let Some(a) = self.adapter() else { return };
        let (path, id, on) = (a.path.clone(), a.id.clone(), !a.discoverable);
        let client = self.client.clone();
        self.spawn(
            "updating adapter…",
            format!("{id} is now {}", if on { "visible to other devices" } else { "hidden" }),
            async move { client.set_discoverable(&path, on).await },
        );
    }

    fn toggle_pairable(&mut self) {
        let Some(a) = self.adapter() else { return };
        let (path, id, on) = (a.path.clone(), a.id.clone(), !a.pairable);
        let client = self.client.clone();
        self.spawn(
            "updating adapter…",
            format!("{id} pairable {}", on_off(on)),
            async move { client.set_pairable(&path, on).await },
        );
    }

    fn toggle_trusted(&mut self) {
        let Some(d) = self.device() else { return };
        let (path, name, on) = (d.path.clone(), d.display_name(), !d.trusted);
        let client = self.client.clone();
        self.spawn(
            "updating device…",
            format!("{name} is now {}", if on { "trusted" } else { "untrusted" }),
            async move { client.set_trusted(&path, on).await },
        );
    }

    fn toggle_blocked(&mut self) {
        let Some(d) = self.device() else { return };
        let (path, name, on) = (d.path.clone(), d.display_name(), !d.blocked);
        let client = self.client.clone();
        self.spawn(
            "updating device…",
            format!("{name} is now {}", if on { "blocked" } else { "unblocked" }),
            async move { client.set_blocked(&path, on).await },
        );
    }

    fn disconnect(&mut self) {
        let Some(d) = self.device() else { return };
        if !d.connected {
            self.set_status(Status::info(format!("{} is not connected", d.display_name())));
            return;
        }
        let (path, name) = (d.path.clone(), d.display_name());
        let client = self.client.clone();
        self.spawn(
            format!("disconnecting {name}…").as_str(),
            format!("{name} disconnected"),
            async move { client.disconnect(&path).await },
        );
    }

    fn forget(&mut self) {
        let Some(a) = self.adapter() else { return };
        let adapter = a.path.clone();
        let Some(d) = self.device() else { return };
        self.modal = Modal::Confirm(ConfirmKind::Forget {
            name: d.display_name(),
            adapter,
            device: d.path.clone(),
        });
    }

    /// Pair first if we have not already, then connect.
    fn connect_selected(&mut self) {
        if self.focus == Focus::Adapters {
            self.focus = Focus::Devices;
            return;
        }
        let Some(a) = self.adapter() else { return };
        if !a.powered {
            self.set_status(Status::error("Bluetooth is off — press w to turn it on"));
            return;
        }
        let Some(d) = self.device() else {
            self.set_status(Status::info("nothing to connect — press s to scan"));
            return;
        };
        let name = d.display_name();
        if d.connected {
            self.set_status(Status::info(format!("already connected to {name}")));
            return;
        }
        if d.blocked {
            self.set_status(Status::info(format!("{name} is blocked — press b to unblock it")));
            return;
        }
        let (path, paired) = (d.path.clone(), d.paired);
        let client = self.client.clone();
        let tx = self.tx.clone();
        let pending = if paired {
            format!("connecting to {name}…")
        } else {
            format!("pairing with {name}…")
        };
        self.spawn(pending.as_str(), format!("connected to {name}"), async move {
            let result = client.pair_and_connect(&path, paired).await;
            // Take down any "type this passkey" card the pairing left up.
            let _ = tx.send(Msg::AgentDone).await;
            result
        });
    }

    fn rename(&mut self) {
        match self.focus {
            Focus::Adapters => {
                let Some(a) = self.adapter() else { return };
                self.modal = Modal::Prompt(Prompt {
                    title: format!("Rename {}", a.id),
                    hint: "the name other devices see when this computer is visible".into(),
                    fields: vec![Field { label: "Name", value: a.alias.clone(), secret: false }],
                    focus: 0,
                    reveal: true,
                    kind: PromptKind::RenameAdapter { adapter: a.path.clone() },
                });
            }
            Focus::Devices => {
                let Some(d) = self.device() else { return };
                self.modal = Modal::Prompt(Prompt {
                    title: format!("Rename {}", d.display_name()),
                    hint: if d.name.is_empty() {
                        "a local nickname for this device".into()
                    } else {
                        format!("a local nickname; the device calls itself “{}”", d.name)
                    },
                    fields: vec![Field { label: "Name", value: d.alias.clone(), secret: false }],
                    focus: 0,
                    reveal: true,
                    kind: PromptKind::RenameDevice { device: d.path.clone() },
                });
            }
        }
    }

    fn submit_prompt(&mut self, p: Prompt) {
        let value = p.fields[0].value.trim().to_string();
        let passkey = (value.len() == 6 && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<u32>().ok())
            .flatten();
        let problem = match &p.kind {
            PromptKind::RenameDevice { .. } | PromptKind::RenameAdapter { .. } if value.is_empty() => {
                Some("a name is required")
            }
            PromptKind::AgentPin(_) if value.is_empty() || value.len() > 16 => {
                Some("a PIN of 1 to 16 characters is required")
            }
            PromptKind::AgentPasskey(_) if passkey.is_none() => Some("the passkey is six digits"),
            _ => None,
        };
        if let Some(why) = problem {
            self.modal = Modal::Prompt(reject(p, why));
            return;
        }

        let client = self.client.clone();
        match p.kind {
            PromptKind::RenameDevice { device } => {
                self.spawn("renaming…", format!("renamed to {value}"), async move {
                    client.set_device_alias(&device, &value).await
                });
            }
            PromptKind::RenameAdapter { adapter } => {
                self.spawn("renaming…", format!("renamed to {value}"), async move {
                    client.set_adapter_alias(&adapter, &value).await
                });
            }
            PromptKind::AgentPin(reply) => {
                let _ = reply.send(AgentReply::Pin(value));
            }
            PromptKind::AgentPasskey(reply) => {
                let _ = reply.send(AgentReply::Passkey(passkey.unwrap_or_default()));
            }
        }
    }
}

// ----------------------------------------------------------------- helpers

fn clamp_move(current: usize, delta: isize, len: usize) -> usize {
    let next = current as isize + delta;
    next.clamp(0, len as isize - 1) as usize
}

fn on_off(v: bool) -> &'static str {
    if v { "on" } else { "off" }
}

fn reject(mut p: Prompt, why: &str) -> Prompt {
    p.hint = why.to_string();
    p
}

/// D-Bus errors arrive as an interface-qualified name plus a sentence; keep the
/// sentence and the part of the name that identifies the failure, and drop the
/// `org.bluez.Error.` boilerplate that only costs the status bar room.
pub fn format_error(e: &anyhow::Error) -> String {
    let context = e.to_string();
    let root = e
        .chain()
        .last()
        .map(|c| c.to_string())
        .unwrap_or_default();

    let mut msg = if root.is_empty() || root == context {
        context
    } else {
        format!("{context}: {root}")
    };
    for noise in [
        "org.bluez.Error.",
        "org.freedesktop.DBus.Error.",
    ] {
        msg = msg.replace(noise, "");
    }
    // BlueZ's connection failures name the profile in a code, not a sentence.
    for (code, sentence) in [
        ("br-connection-profile-unavailable", "no usable profile — is the device in pairing mode?"),
        ("br-connection-page-timeout", "the device did not answer — is it on and in range?"),
        ("br-connection-canceled", "connection cancelled"),
        ("br-connection-key-missing", "the device forgot this computer — forget it here and pair again"),
        ("le-connection-abort-by-local", "connection aborted"),
        ("br-connection-adapter-not-powered", "the adapter is off"),
        ("br-connection-busy", "the device is busy"),
    ] {
        msg = msg.replace(code, sentence);
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_keeps_the_action_and_drops_the_bus_boilerplate() {
        let root = anyhow::anyhow!("org.bluez.Error.Failed: br-connection-page-timeout");
        let e = root.context("connecting");
        assert_eq!(
            format_error(&e),
            "connecting: Failed: the device did not answer — is it on and in range?"
        );
    }

    #[test]
    fn error_without_a_cause_is_not_doubled_up() {
        let e = anyhow::anyhow!("pairing timed out");
        assert_eq!(format_error(&e), "pairing timed out");
    }

    #[test]
    fn movement_is_clamped_to_the_list() {
        assert_eq!(clamp_move(0, -1, 5), 0);
        assert_eq!(clamp_move(4, 1, 5), 4);
        assert_eq!(clamp_move(2, 10, 5), 4);
        assert_eq!(clamp_move(2, isize::MIN / 2, 5), 0);
    }
}

//! The pairing agent BlueZ calls back into when a device needs a PIN, a
//! passkey, or a yes/no.
//!
//! BlueZ does not accept secrets up front the way NetworkManager does; it asks
//! for them mid-pairing, over D-Bus, on an object *we* export. Each request is
//! forwarded to the UI as a message carrying a one-shot reply channel, and the
//! method call stays pending until the user answers or BlueZ cancels.

use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use zbus::zvariant::OwnedObjectPath;
use zbus::{DBusError, interface};

use crate::app::Msg;

/// BlueZ's own answer window is 60 seconds; we give up a little later so the
/// daemon, not us, is the one to report the timeout.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

/// What BlueZ wants from the user.
#[derive(Debug, Clone)]
pub enum AgentRequestKind {
    /// Type the PIN shown on (or printed on) the device: legacy pairing.
    PinCode,
    /// Type the 6-digit passkey the device is showing.
    Passkey,
    /// Confirm that both sides show the same 6-digit passkey.
    Confirm { passkey: u32 },
    /// Allow an incoming pairing that needs no code.
    Authorize,
    /// Allow a paired-but-untrusted device to use a profile.
    AuthorizeService { uuid: String },
}

#[derive(Debug)]
pub struct AgentRequest {
    pub device: OwnedObjectPath,
    pub kind: AgentRequestKind,
    pub reply: oneshot::Sender<AgentReply>,
}

#[derive(Debug, Clone)]
pub enum AgentReply {
    Pin(String),
    Passkey(u32),
    Accept,
    Reject,
}

pub struct Agent {
    tx: mpsc::Sender<Msg>,
}

impl Agent {
    pub fn new(tx: mpsc::Sender<Msg>) -> Self {
        Self { tx }
    }

    async fn ask(&self, device: OwnedObjectPath, kind: AgentRequestKind) -> Result<AgentReply, AgentError> {
        let (reply, answer) = oneshot::channel();
        let req = AgentRequest { device, kind, reply };
        if self.tx.send(Msg::Agent(req)).await.is_err() {
            return Err(AgentError::Canceled("the manager is shutting down".into()));
        }
        match tokio::time::timeout(ANSWER_TIMEOUT, answer).await {
            Ok(Ok(AgentReply::Reject)) => Err(AgentError::Rejected("rejected by the user".into())),
            Ok(Ok(reply)) => Ok(reply),
            // The UI dropped the channel: a newer request replaced this one.
            Ok(Err(_)) => Err(AgentError::Canceled("request superseded".into())),
            Err(_) => Err(AgentError::Canceled("no answer from the user".into())),
        }
    }
}

#[interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self) {}

    async fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, AgentError> {
        match self.ask(device, AgentRequestKind::PinCode).await? {
            AgentReply::Pin(pin) => Ok(pin),
            _ => Err(AgentError::Rejected("no PIN entered".into())),
        }
    }

    async fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) {
        let _ = self
            .tx
            .send(Msg::AgentDisplay { device, text: format!("enter PIN {pincode} on the device") })
            .await;
    }

    async fn request_passkey(&self, device: OwnedObjectPath) -> Result<u32, AgentError> {
        match self.ask(device, AgentRequestKind::Passkey).await? {
            AgentReply::Passkey(key) => Ok(key),
            _ => Err(AgentError::Rejected("no passkey entered".into())),
        }
    }

    async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, entered: u16) {
        let mut text = format!("enter passkey {passkey:06} on the device");
        if entered > 0 {
            text.push_str(&format!("  ({entered} typed)"));
        }
        let _ = self.tx.send(Msg::AgentDisplay { device, text }).await;
    }

    async fn request_confirmation(&self, device: OwnedObjectPath, passkey: u32) -> Result<(), AgentError> {
        self.ask(device, AgentRequestKind::Confirm { passkey }).await.map(drop)
    }

    async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), AgentError> {
        self.ask(device, AgentRequestKind::Authorize).await.map(drop)
    }

    async fn authorize_service(&self, device: OwnedObjectPath, uuid: String) -> Result<(), AgentError> {
        self.ask(device, AgentRequestKind::AuthorizeService { uuid }).await.map(drop)
    }

    async fn cancel(&self) {
        let _ = self.tx.send(Msg::AgentCancel).await;
    }
}

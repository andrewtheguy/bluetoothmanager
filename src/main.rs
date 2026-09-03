//! A terminal Bluetooth manager for BlueZ, driving its D-Bus API directly.

mod app;
mod bluez;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::EventStream;
use futures_util::StreamExt;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use zbus::message::Type as MessageType;
use zbus::{MatchRule, MessageStream};

use app::{App, Msg, Status};
use bluez::BluezClient;

/// Fall back to a poll at this interval when the bus is quiet.
const IDLE_REFRESH: Duration = Duration::from_secs(3);
/// Floor between refreshes, so a burst of signals cannot spin the client.
const MIN_REFRESH_GAP: Duration = Duration::from_millis(400);
/// Let a burst settle before reading, so we snapshot a consistent state.
const DEBOUNCE: Duration = Duration::from_millis(120);

const USAGE: &str = "\
bluetoothmanager — a terminal Bluetooth manager for BlueZ

usage: bluetoothmanager

Talks to bluetoothd over D-Bus. Takes no options; press ? inside for keys.
Pairing prompts (PINs, passkeys, confirmations) open as dialogs; the manager
registers itself as the default pairing agent while it runs.";

#[tokio::main]
async fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        Some("-V" | "--version") => {
            println!("bluetoothmanager {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some(other) => {
            eprintln!("bluetoothmanager: unexpected argument `{other}`\n\n{USAGE}");
            std::process::exit(2);
        }
    }

    let (tx, mut rx) = mpsc::channel::<Msg>(64);
    let (poke_tx, poke_rx) = mpsc::channel::<()>(8);

    let client = match BluezClient::new(tx.clone()).await {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("bluetoothmanager: {}", app::format_error(&e));
            eprintln!("is bluetoothd running? (systemctl status bluetooth)");
            std::process::exit(1);
        }
    };

    tokio::spawn(refresher(client.clone(), tx.clone(), poke_rx));

    let mut app = App::new(client, tx, poke_tx);
    // Leave the terminal usable when we are killed or the terminal goes away,
    // not only on a clean `q`.
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;
    let mut terminal = ratatui::init();
    let mut events = EventStream::new();

    let result = loop {
        if let Err(e) = terminal.draw(|f| ui::draw(f, &app)) {
            break Err(e.into());
        }
        if app.quit {
            break Ok(());
        }

        tokio::select! {
            ev = events.next() => match ev {
                Some(Ok(ev)) => app.on_event(ev),
                Some(Err(e)) => break Err(e.into()),
                None => break Ok(()),
            },
            msg = rx.recv() => match msg {
                Some(msg) => app.on_msg(msg),
                None => break Ok(()),
            },
            _ = sigterm.recv() => break Ok(()),
            _ = sighup.recv() => break Ok(()),
            // Keeps the spinner turning and ages out stale status messages.
            _ = tokio::time::sleep(Duration::from_millis(120)) => {}
        }
    };

    ratatui::restore();
    result
}

/// Publishes snapshots of BlueZ's state to the UI.
///
/// BlueZ announces everything we care about — adapters and devices appearing,
/// connection and pairing state, RSSI — through `ObjectManager` and
/// `PropertiesChanged` signals, so refreshes are driven by the bus rather than
/// by a fixed poll; the timer is only a safety net.
async fn refresher(client: Arc<BluezClient>, tx: mpsc::Sender<Msg>, mut poke: mpsc::Receiver<()>) {
    let (wake_tx, mut wake_rx) = mpsc::channel::<()>(1);
    tokio::spawn(watch_signals(client.clone(), wake_tx));

    // A bus that is down fails every refresh; reporting the same sentence twice
    // a second only costs the user the status message they were reading.
    let mut last_error: Option<String> = None;

    loop {
        let started = tokio::time::Instant::now();
        match client.snapshot().await {
            Ok(snap) => {
                last_error = None;
                if tx.send(Msg::Snapshot(Box::new(snap))).await.is_err() {
                    return;
                }
            }
            Err(e) => {
                let text = app::format_error(&e);
                if last_error.as_deref() != Some(text.as_str()) {
                    last_error = Some(text.clone());
                    let _ = tx.send(Msg::Status(Status::error(text))).await;
                }
            }
        }

        tokio::select! {
            _ = poke.recv() => {}
            _ = wake_rx.recv() => {}
            _ = tokio::time::sleep(IDLE_REFRESH) => {}
        }

        // Let a burst settle, and keep a floor under the refresh rate: a
        // discovery session reports RSSI faster than a snapshot takes to read.
        tokio::time::sleep(DEBOUNCE).await;
        tokio::time::sleep_until(started + MIN_REFRESH_GAP).await;
    }
}

/// Collapse BlueZ's signal traffic into a single wake-up.
///
/// This has to run in its own task and consume messages as fast as they arrive.
/// A `MessageStream` that is left unread back-pressures the shared connection,
/// which stalls the replies to our own method calls — and a discovery session
/// with a dozen advertisers in range fills the queue in well under a second.
async fn watch_signals(client: Arc<BluezClient>, wake: mpsc::Sender<()>) {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender("org.bluez")
        .expect("valid bus name")
        .build();
    let Ok(mut signals) = MessageStream::for_match_rule(rule, client.connection(), Some(64)).await
    else {
        return;
    };
    while signals.next().await.is_some() {
        // A full channel already means "refresh pending", so dropping the
        // notification is exactly right.
        let _ = wake.try_send(());
    }
}

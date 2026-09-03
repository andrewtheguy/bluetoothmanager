//! High level operations on top of the BlueZ D-Bus API.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::sync::mpsc;
use zbus::Connection;
use zbus::fdo::ObjectManagerProxy;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::agent::Agent;
use super::proxies::*;
use super::types::*;
use crate::app::Msg;

/// How long to give a `Connect` before we stop waiting on BlueZ. The daemon
/// has its own timeouts, but a device that vanished mid-handshake can leave a
/// call hanging well past what anyone wants to watch a spinner for.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
/// Pairing includes the user typing a passkey, so it gets more room.
const PAIR_TIMEOUT: Duration = Duration::from_secs(120);

// ------------------------------------------------------------------ properties

/// One interface's slice of `GetManagedObjects`, with lenient typed accessors:
/// a property BlueZ did not include simply reads as a default. BlueZ omits
/// optional properties (RSSI, Name, Battery) rather than sending placeholders,
/// so this is the normal case, not an error.
pub struct Props<'a>(&'a HashMap<String, OwnedValue>);

impl Props<'_> {
    fn get<T>(&self, key: &str) -> Option<T>
    where
        T: TryFrom<OwnedValue>,
    {
        self.0.get(key).and_then(|v| T::try_from(v.clone()).ok())
    }

    pub fn u32(&self, key: &str) -> u32 {
        self.get(key).unwrap_or(0)
    }

    pub fn u16(&self, key: &str) -> u16 {
        self.get(key).unwrap_or(0)
    }

    pub fn u8(&self, key: &str) -> u8 {
        self.get(key).unwrap_or(0)
    }

    pub fn opt_u8(&self, key: &str) -> Option<u8> {
        self.get(key)
    }

    pub fn opt_i16(&self, key: &str) -> Option<i16> {
        self.get(key)
    }

    pub fn bool(&self, key: &str) -> bool {
        self.get(key).unwrap_or(false)
    }

    pub fn string(&self, key: &str) -> String {
        self.get(key).unwrap_or_default()
    }

    pub fn strings(&self, key: &str) -> Vec<String> {
        self.get(key).unwrap_or_default()
    }

    pub fn path(&self, key: &str) -> Option<OwnedObjectPath> {
        self.get(key)
    }

    /// The keys of an `a{qv}` dictionary, as used by `ManufacturerData`.
    pub fn u16_keys(&self, key: &str) -> Vec<u16> {
        self.0
            .get(key)
            .and_then(|v| v.downcast_ref::<zbus::zvariant::Dict>().ok())
            .map(|d| {
                d.iter()
                    .filter_map(|(k, _)| k.downcast_ref::<u16>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

// --------------------------------------------------------------------- client

pub struct BluezClient {
    conn: Connection,
    objects: ObjectManagerProxy<'static>,
}

impl BluezClient {
    /// Connect to the bus, export the pairing agent, and register it as the
    /// default so BlueZ routes every pairing prompt to us.
    pub async fn new(tx: mpsc::Sender<Msg>) -> Result<Self> {
        let conn = Connection::system()
            .await
            .context("connecting to the system bus")?;
        let objects = ObjectManagerProxy::builder(&conn)
            .destination(BLUEZ_SERVICE)?
            .path("/")?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        // Fail fast if bluetoothd is not there, before the UI comes up.
        objects
            .get_managed_objects()
            .await
            .context("reaching org.bluez")?;

        conn.object_server()
            .at(AGENT_PATH, Agent::new(tx))
            .await
            .context("exporting the pairing agent")?;
        let agents = AgentManagerProxy::builder(&conn)
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        let agent_path = ObjectPath::try_from(AGENT_PATH)?;
        agents
            .register_agent(&agent_path, "KeyboardDisplay")
            .await
            .context("registering the pairing agent")?;
        // Being the default is what makes *incoming* pairings land on us too.
        // Another agent may already hold it; that only costs us those prompts.
        let _ = agents.request_default_agent(&agent_path).await;

        Ok(Self { conn, objects })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    // ------------------------------------------------------------- read state

    /// One round trip: BlueZ publishes every adapter and device, with all their
    /// properties, through `ObjectManager.GetManagedObjects`.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let objects = self
            .objects
            .get_managed_objects()
            .await
            .context("reading BlueZ state")?;

        let mut adapters: Vec<Adapter> = objects
            .iter()
            .filter_map(|(path, ifaces)| {
                let a = ifaces.get(IFACE_ADAPTER)?;
                Some(read_adapter(path, Props(a)))
            })
            .collect();

        for (path, ifaces) in &objects {
            let Some(d) = ifaces.get(IFACE_DEVICE) else { continue };
            let battery = ifaces.get(IFACE_BATTERY).map(|b| Props(b).u8("Percentage"));
            let media_connected = ifaces
                .get(IFACE_MEDIA_CONTROL)
                .is_some_and(|m| Props(m).bool("Connected"));
            let device = read_device(path, Props(d), battery, media_connected);
            let Some(adapter_path) = Props(d).path("Adapter") else { continue };
            if let Some(adapter) = adapters.iter_mut().find(|a| a.path == adapter_path) {
                adapter.devices.push(device);
            }
        }

        for adapter in &mut adapters {
            sort_devices(&mut adapter.devices);
        }
        adapters.sort_by(|a, b| a.id.cmp(&b.id));

        Ok(Snapshot { adapters })
    }

    // ---------------------------------------------------------------- adapter

    pub async fn set_powered(&self, adapter: &OwnedObjectPath, on: bool) -> Result<()> {
        self.adapter(adapter)
            .await?
            .set_powered(on)
            .await
            .context(if on { "powering on" } else { "powering off" })
    }

    pub async fn set_discoverable(&self, adapter: &OwnedObjectPath, on: bool) -> Result<()> {
        self.adapter(adapter)
            .await?
            .set_discoverable(on)
            .await
            .context("setting discoverable")
    }

    pub async fn set_pairable(&self, adapter: &OwnedObjectPath, on: bool) -> Result<()> {
        self.adapter(adapter)
            .await?
            .set_pairable(on)
            .await
            .context("setting pairable")
    }

    pub async fn set_adapter_alias(&self, adapter: &OwnedObjectPath, alias: &str) -> Result<()> {
        self.adapter(adapter)
            .await?
            .set_alias(alias)
            .await
            .context("renaming the adapter")
    }

    /// Discovery is a per-client session in BlueZ: it runs until we stop it or
    /// disconnect from the bus, and RSSI updates stream in the whole time.
    pub async fn start_discovery(&self, adapter: &OwnedObjectPath) -> Result<()> {
        let proxy = self.adapter(adapter).await?;
        let mut filter: HashMap<String, Value<'_>> = HashMap::new();
        filter.insert("Transport".into(), Value::from("auto"));
        // Report every RSSI change rather than only the first sighting, so the
        // signal column stays live while the user picks a device.
        filter.insert("DuplicateData".into(), Value::from(true));
        proxy
            .set_discovery_filter(filter)
            .await
            .context("setting the discovery filter")?;
        match proxy.start_discovery().await {
            Ok(()) => Ok(()),
            // Already running under our session: that is the state we wanted.
            Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.bluez.Error.InProgress" => Ok(()),
            Err(e) => Err(e).context("starting discovery"),
        }
    }

    pub async fn stop_discovery(&self, adapter: &OwnedObjectPath) -> Result<()> {
        self.adapter(adapter)
            .await?
            .stop_discovery()
            .await
            .context("stopping discovery")
    }

    pub async fn remove_device(&self, adapter: &OwnedObjectPath, device: &OwnedObjectPath) -> Result<()> {
        self.adapter(adapter)
            .await?
            .remove_device(&device.as_ref())
            .await
            .context("removing the device")
    }

    // ----------------------------------------------------------------- device

    /// Pair, mark trusted so the device may reconnect on its own, then connect.
    /// A device that is already paired skips straight to the connection.
    pub async fn pair_and_connect(&self, device: &OwnedObjectPath, already_paired: bool) -> Result<()> {
        let proxy = self.device(device).await?;
        if !already_paired {
            match tokio::time::timeout(PAIR_TIMEOUT, proxy.pair()).await {
                Ok(Ok(())) => {}
                Ok(Err(zbus::Error::MethodError(name, _, _)))
                    if name.as_str() == "org.bluez.Error.AlreadyExists" => {}
                Ok(Err(e)) => return Err(e).context("pairing"),
                Err(_) => {
                    let _ = proxy.cancel_pairing().await;
                    bail!("pairing timed out");
                }
            }
            proxy.set_trusted(true).await.context("trusting the device")?;
        }
        match tokio::time::timeout(CONNECT_TIMEOUT, proxy.connect()).await {
            Ok(r) => r.context("connecting"),
            Err(_) => bail!("connection timed out"),
        }
    }

    pub async fn disconnect(&self, device: &OwnedObjectPath) -> Result<()> {
        self.device(device)
            .await?
            .disconnect()
            .await
            .context("disconnecting")
    }

    pub async fn set_trusted(&self, device: &OwnedObjectPath, on: bool) -> Result<()> {
        self.device(device)
            .await?
            .set_trusted(on)
            .await
            .context("updating trust")
    }

    pub async fn set_blocked(&self, device: &OwnedObjectPath, on: bool) -> Result<()> {
        self.device(device)
            .await?
            .set_blocked(on)
            .await
            .context("updating block")
    }

    pub async fn set_device_alias(&self, device: &OwnedObjectPath, alias: &str) -> Result<()> {
        self.device(device)
            .await?
            .set_alias(alias)
            .await
            .context("renaming the device")
    }

    async fn adapter(&self, path: &OwnedObjectPath) -> Result<AdapterProxy<'static>> {
        Ok(AdapterProxy::builder(&self.conn)
            .path(path.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?)
    }

    async fn device(&self, path: &OwnedObjectPath) -> Result<DeviceProxy<'static>> {
        Ok(DeviceProxy::builder(&self.conn)
            .path(path.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?)
    }
}

// -------------------------------------------------------------------- readers

fn read_adapter(path: &OwnedObjectPath, p: Props<'_>) -> Adapter {
    let id = path.as_str().rsplit('/').next().unwrap_or_default().to_string();
    Adapter {
        path: path.clone(),
        model: adapter_model(&id),
        id,
        name: p.string("Name"),
        alias: p.string("Alias"),
        address: p.string("Address"),
        address_type: p.string("AddressType"),
        powered: p.bool("Powered"),
        power_state: PowerState::parse(&p.string("PowerState")),
        discoverable: p.bool("Discoverable"),
        discoverable_timeout: p.u32("DiscoverableTimeout"),
        pairable: p.bool("Pairable"),
        discovering: p.bool("Discovering"),
        connectable: p.bool("Connectable"),
        roles: p.strings("Roles"),
        uuids: p.strings("UUIDs"),
        modalias: p.string("Modalias"),
        manufacturer: p.u16("Manufacturer"),
        version: p.u8("Version"),
        devices: Vec::new(),
    }
}

fn read_device(
    path: &OwnedObjectPath,
    p: Props<'_>,
    battery: Option<u8>,
    media_connected: bool,
) -> Device {
    Device {
        path: path.clone(),
        address: p.string("Address"),
        address_type: p.string("AddressType"),
        name: p.string("Name"),
        alias: p.string("Alias"),
        icon: p.string("Icon"),
        class: p.u32("Class"),
        appearance: p.u16("Appearance"),
        paired: p.bool("Paired"),
        bonded: p.bool("Bonded"),
        trusted: p.bool("Trusted"),
        blocked: p.bool("Blocked"),
        connected: p.bool("Connected"),
        legacy_pairing: p.bool("LegacyPairing"),
        services_resolved: p.bool("ServicesResolved"),
        wake_allowed: p.bool("WakeAllowed"),
        rssi: p.opt_i16("RSSI"),
        tx_power: p.opt_i16("TxPower"),
        uuids: p.strings("UUIDs"),
        manufacturer_ids: p.u16_keys("ManufacturerData"),
        battery: battery.or_else(|| p.opt_u8("BatteryPercentage")),
        media_connected,
    }
}

/// BlueZ only reports the chip vendor, so the model comes from the kernel:
/// `/sys/class/bluetooth/hciN/device` is the USB interface (or PCI function)
/// the controller hangs off, and USB devices carry their product string one
/// level up. Anything without one — PCI, SDIO, UART controllers — yields
/// nothing, and the caller falls back to the vendor name.
fn adapter_model(id: &str) -> String {
    let device = std::path::Path::new("/sys/class/bluetooth").join(id).join("device");
    let read = |dir: &std::path::Path, name: &str| {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    for dir in [device.join(".."), device] {
        let Some(product) = read(&dir, "product") else { continue };
        return match read(&dir, "manufacturer") {
            Some(maker) if !repeats_maker(&maker, &product) => {
                format!("{} {product}", brand(&maker))
            }
            _ => product,
        };
    }
    String::new()
}

/// "Apple Inc." + "Bluetooth USB Host Controller" reads better joined;
/// "TP-Link" + "TP-Link UB500 Adapter" does not.
fn repeats_maker(maker: &str, product: &str) -> bool {
    product.to_lowercase().starts_with(&brand(maker).to_lowercase())
}

/// The manufacturer string without its corporate suffix: "Apple Inc." → "Apple".
fn brand(maker: &str) -> &str {
    let mut out = maker.trim();
    for suffix in [", Inc.", " Inc.", " Inc", " Corp.", " Corporation", " Co., Ltd.", " Ltd.", " Ltd", " GmbH", " LLC"] {
        if let Some(stripped) = out.strip_suffix(suffix) {
            out = stripped.trim_end_matches(',').trim();
            break;
        }
    }
    out
}

/// Connected devices first, then paired ones, then whatever is in range with
/// the strongest signal on top. Nameless advertisers sink to the bottom: they
/// are mostly beacons and phones with randomised addresses.
fn sort_devices(devices: &mut [Device]) {
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(a.name.is_empty().cmp(&b.name.is_empty()))
            .then(b.rssi.unwrap_or(i16::MIN).cmp(&a.rssi.unwrap_or(i16::MIN)))
            .then(a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()))
    });
}

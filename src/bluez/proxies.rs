//! zbus proxies for the slice of the BlueZ D-Bus API we drive.
//!
//! Bulk property *reads* come from a single `ObjectManager.GetManagedObjects`
//! call (see `client::snapshot`), so these proxies carry only the method calls
//! and the writable properties.

use std::collections::HashMap;

use zbus::proxy;
use zbus::zvariant::{ObjectPath, Value};

pub const BLUEZ_SERVICE: &str = "org.bluez";

pub const IFACE_ADAPTER: &str = "org.bluez.Adapter1";
pub const IFACE_DEVICE: &str = "org.bluez.Device1";
pub const IFACE_BATTERY: &str = "org.bluez.Battery1";
pub const IFACE_MEDIA_CONTROL: &str = "org.bluez.MediaControl1";

/// The object path our pairing agent lives at, on our own connection.
pub const AGENT_PATH: &str = "/org/bluetoothmanager/agent";

#[proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
pub trait Adapter {
    fn start_discovery(&self) -> zbus::Result<()>;

    fn stop_discovery(&self) -> zbus::Result<()>;

    fn set_discovery_filter(&self, filter: HashMap<String, Value<'_>>) -> zbus::Result<()>;

    fn remove_device(&self, device: &ObjectPath<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_powered(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_discoverable(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_pairable(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_alias(&self, value: &str) -> zbus::Result<()>;
}

#[proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
pub trait Device {
    fn connect(&self) -> zbus::Result<()>;

    fn disconnect(&self) -> zbus::Result<()>;

    fn connect_profile(&self, uuid: &str) -> zbus::Result<()>;

    fn disconnect_profile(&self, uuid: &str) -> zbus::Result<()>;

    fn pair(&self) -> zbus::Result<()>;

    fn cancel_pairing(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_trusted(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_blocked(&self, value: bool) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_alias(&self, value: &str) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.bluez.AgentManager1",
    default_service = "org.bluez",
    default_path = "/org/bluez"
)]
pub trait AgentManager {
    fn register_agent(&self, agent: &ObjectPath<'_>, capability: &str) -> zbus::Result<()>;

    fn unregister_agent(&self, agent: &ObjectPath<'_>) -> zbus::Result<()>;

    fn request_default_agent(&self, agent: &ObjectPath<'_>) -> zbus::Result<()>;
}

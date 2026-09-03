//! Plain data read off the bus, plus the lookup tables that make it legible.

use std::fmt;

use zbus::zvariant::OwnedObjectPath;

// ------------------------------------------------------------------- adapter

/// `Adapter1.PowerState`, which is finer-grained than `Powered` and is the only
/// place BlueZ tells us the radio is rfkill-blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerState {
    #[default]
    Unknown,
    On,
    Off,
    OffEnabling,
    OnDisabling,
    OffBlocked,
}

impl PowerState {
    pub fn parse(s: &str) -> Self {
        match s {
            "on" => Self::On,
            "off" => Self::Off,
            "off-enabling" => Self::OffEnabling,
            "on-disabling" => Self::OnDisabling,
            "off-blocked" => Self::OffBlocked,
            _ => Self::Unknown,
        }
    }

    pub fn is_busy(self) -> bool {
        matches!(self, Self::OffEnabling | Self::OnDisabling)
    }
}

impl fmt::Display for PowerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown",
            Self::On => "on",
            Self::Off => "off",
            Self::OffEnabling => "powering on",
            Self::OnDisabling => "powering off",
            Self::OffBlocked => "blocked",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Adapter {
    pub path: OwnedObjectPath,
    /// The `hciN` name, taken from the object path.
    pub id: String,
    pub name: String,
    pub alias: String,
    pub address: String,
    pub address_type: String,
    /// What the hardware calls itself, from sysfs: "TP-Link UB500 Adapter".
    /// Empty when the bus type exposes no product string.
    pub model: String,
    pub powered: bool,
    pub power_state: PowerState,
    pub discoverable: bool,
    pub discoverable_timeout: u32,
    pub pairable: bool,
    pub discovering: bool,
    pub connectable: bool,
    pub roles: Vec<String>,
    pub uuids: Vec<String>,
    pub modalias: String,
    pub manufacturer: u16,
    pub version: u8,
    pub devices: Vec<Device>,
}

impl Adapter {
    /// The name to list the adapter under: the hardware model when the kernel
    /// knows it, else the chip vendor, else the bare `hciN`.
    pub fn label(&self) -> String {
        if !self.model.is_empty() {
            self.model.clone()
        } else if let Some(vendor) = company_name(self.manufacturer) {
            format!("{vendor} adapter")
        } else {
            self.id.clone()
        }
    }

    pub fn connected_devices(&self) -> impl Iterator<Item = &Device> {
        self.devices.iter().filter(|d| d.connected)
    }

    pub fn blocked(&self) -> bool {
        self.power_state == PowerState::OffBlocked
    }
}

// -------------------------------------------------------------------- device

#[derive(Debug, Clone)]
pub struct Device {
    pub path: OwnedObjectPath,
    pub address: String,
    pub address_type: String,
    pub name: String,
    pub alias: String,
    pub icon: String,
    pub class: u32,
    pub appearance: u16,
    pub paired: bool,
    pub bonded: bool,
    pub trusted: bool,
    pub blocked: bool,
    pub connected: bool,
    pub legacy_pairing: bool,
    pub services_resolved: bool,
    pub wake_allowed: bool,
    pub rssi: Option<i16>,
    pub tx_power: Option<i16>,
    pub uuids: Vec<String>,
    pub manufacturer_ids: Vec<u16>,
    pub battery: Option<u8>,
    pub media_connected: bool,
}

impl Device {
    /// What to call the device in lists: the user's alias, else the name it
    /// advertises, else its address.
    pub fn display_name(&self) -> String {
        if !self.alias.is_empty() {
            self.alias.clone()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            self.address.clone()
        }
    }

    pub fn kind(&self) -> DeviceKind {
        DeviceKind::of(&self.icon, self.class, self.appearance)
    }

    /// Names of the profiles this device offers that are worth a line in the UI.
    pub fn profile_names(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = self.uuids.iter().filter_map(|u| profile_name(u)).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn is_le(&self) -> bool {
        self.address_type == "random" || (self.appearance != 0 && self.class == 0)
    }
}

/// A coarse device category, for an icon and a sort bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Headset,
    Headphones,
    Speaker,
    Keyboard,
    Mouse,
    Gamepad,
    Phone,
    Computer,
    Watch,
    Tv,
    Printer,
    Camera,
    Health,
    Network,
    Other,
}

impl DeviceKind {
    /// BlueZ's own `Icon` property is the best guess when present; otherwise
    /// decode the classic Class of Device, then the LE appearance.
    pub fn of(icon: &str, class: u32, appearance: u16) -> Self {
        match icon {
            "audio-headset" => return Self::Headset,
            "audio-headphones" => return Self::Headphones,
            "audio-card" | "audio-speakers" | "multimedia-player" => return Self::Speaker,
            "input-keyboard" => return Self::Keyboard,
            "input-mouse" | "input-tablet" => return Self::Mouse,
            "input-gaming" => return Self::Gamepad,
            "phone" => return Self::Phone,
            "computer" => return Self::Computer,
            "video-display" => return Self::Tv,
            "printer" => return Self::Printer,
            "camera-photo" | "camera-video" => return Self::Camera,
            "network-wireless" | "modem" => return Self::Network,
            _ => {}
        }
        if class != 0 {
            let major = (class >> 8) & 0x1f;
            let minor = (class >> 2) & 0x3f;
            return match major {
                0x01 => Self::Computer,
                0x02 => Self::Phone,
                0x03 => Self::Network,
                0x04 => match minor {
                    0x01..=0x03 => Self::Headset,
                    0x06 => Self::Headphones,
                    0x0b | 0x0c | 0x0f => Self::Tv,
                    0x0d => Self::Camera,
                    _ => Self::Speaker,
                },
                0x05 => match minor & 0x30 {
                    0x10 => Self::Keyboard,
                    0x20 => Self::Mouse,
                    0x30 => Self::Keyboard,
                    _ => match minor & 0x0f {
                        0x01 | 0x02 => Self::Gamepad,
                        _ => Self::Other,
                    },
                },
                0x06 => match minor {
                    _ if minor & 0x20 != 0 => Self::Printer,
                    _ if minor & 0x08 != 0 || minor & 0x10 != 0 => Self::Camera,
                    _ => Self::Other,
                },
                0x07 => match minor {
                    0x01 => Self::Watch,
                    _ => Self::Other,
                },
                0x09 => Self::Health,
                _ => Self::Other,
            };
        }
        match appearance >> 6 {
            0x01 => Self::Phone,
            0x02 => Self::Computer,
            0x03 => Self::Watch,
            0x08 => Self::Health,
            0x0d => Self::Health,
            0x0f => match appearance & 0x3f {
                0x01 => Self::Keyboard,
                0x02 => Self::Mouse,
                0x03 | 0x04 => Self::Gamepad,
                _ => Self::Other,
            },
            0x10..=0x14 => Self::Health,
            _ => Self::Other,
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Headset => "🎧",
            Self::Headphones => "🎧",
            Self::Speaker => "🔊",
            Self::Keyboard => "⌨️ ",
            Self::Mouse => "🖱️ ",
            Self::Gamepad => "🎮",
            Self::Phone => "📱",
            Self::Computer => "💻",
            Self::Watch => "⌚",
            Self::Tv => "📺",
            Self::Printer => "🖨️ ",
            Self::Camera => "📷",
            Self::Health => "❤️ ",
            Self::Network => "📡",
            Self::Other => "•",
        }
    }
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Headset => "headset",
            Self::Headphones => "headphones",
            Self::Speaker => "speaker",
            Self::Keyboard => "keyboard",
            Self::Mouse => "mouse",
            Self::Gamepad => "gamepad",
            Self::Phone => "phone",
            Self::Computer => "computer",
            Self::Watch => "watch",
            Self::Tv => "display",
            Self::Printer => "printer",
            Self::Camera => "camera",
            Self::Health => "health",
            Self::Network => "network",
            Self::Other => "device",
        })
    }
}

// --------------------------------------------------------------------- uuids

/// The 16-bit assigned number inside a Bluetooth base UUID, if it is one.
pub fn short_uuid(uuid: &str) -> Option<u16> {
    let uuid = uuid.to_ascii_lowercase();
    let rest = uuid.strip_suffix("-0000-1000-8000-00805f9b34fb")?;
    let head = rest.strip_prefix("0000")?;
    (head.len() == 4).then(|| u16::from_str_radix(head, 16).ok()).flatten()
}

/// The profile or service a UUID stands for, for the ones a user recognises.
pub fn profile_name(uuid: &str) -> Option<&'static str> {
    Some(match short_uuid(uuid)? {
        0x1101 => "serial port",
        0x1105 => "object push",
        0x1106 => "file transfer",
        0x1108 | 0x1112 => "headset",
        0x110a => "audio source",
        0x110b => "audio sink",
        0x110c | 0x110e | 0x110f => "remote control",
        0x110d => "advanced audio",
        0x1115..=0x1117 => "network",
        0x111e | 0x111f => "hands-free",
        0x1124 => "HID",
        0x112d => "SIM access",
        0x112f => "phonebook",
        0x1132..=0x1134 => "messages",
        0x1200 => "PnP info",
        0x1203 => "generic audio",
        0x1800 | 0x1801 => return None,
        0x180a => "device info",
        0x180d => "heart rate",
        0x180f => "battery",
        0x1812 => "HID over GATT",
        0x1816 => "cycling",
        0x1826 => "fitness",
        0x184e | 0x184f | 0x1850 | 0x1854 => "LE audio",
        0xfe2c => "fast pair",
        0xfd6f => "exposure notification",
        _ => return None,
    })
}

/// Short human-readable label for any UUID: the profile name, the assigned
/// number, or the raw string for vendor UUIDs.
pub fn describe_uuid(uuid: &str) -> String {
    match (profile_name(uuid), short_uuid(uuid)) {
        (Some(name), Some(n)) => format!("{name} ({n:04x})"),
        (None, Some(n)) => format!("{n:04x}"),
        (_, None) => uuid.to_string(),
    }
}

/// Manufacturer names for the company IDs found in advertising data, for the
/// handful that show up constantly on a home network.
pub fn company_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0006 => "Microsoft",
        0x000f => "Broadcom",
        0x004c => "Apple",
        0x0059 => "Nordic",
        0x0075 => "Samsung",
        0x0087 => "Garmin",
        0x00e0 => "Google",
        0x0157 => "Huami",
        0x038f => "Xiaomi",
        0x01d7 => "Qualcomm",
        0x0310 => "SGL Italia",
        0x0499 => "Ruuvi",
        0x004f => "Sony",
        0x0131 => "Cypress",
        0x0002 => "Intel",
        0x000a => "Qualcomm",
        0x00d2 => "Bose",
        0x012d => "Sony",
        0x0171 => "Amazon",
        0x0822 => "Anker",
        _ => return None,
    })
}

// -------------------------------------------------------------------- signal

/// Map an RSSI reading to the four-bar scale the UI draws.
pub fn rssi_bars(rssi: i16) -> u8 {
    match rssi {
        r if r >= -60 => 4,
        r if r >= -70 => 3,
        r if r >= -80 => 2,
        _ => 1,
    }
}

// ------------------------------------------------------------------ snapshot

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub adapters: Vec<Adapter>,
}

impl Snapshot {
    pub fn find_device(&self, path: &OwnedObjectPath) -> Option<&Device> {
        self.adapters
            .iter()
            .flat_map(|a| a.devices.iter())
            .find(|d| &d.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_uuids_decode() {
        assert_eq!(short_uuid("0000110b-0000-1000-8000-00805f9b34fb"), Some(0x110b));
        assert_eq!(short_uuid("0000110B-0000-1000-8000-00805F9B34FB"), Some(0x110b));
        assert_eq!(short_uuid("6e400001-b5a3-f393-e0a9-e50e24dcca9e"), None);
        assert_eq!(profile_name("0000110b-0000-1000-8000-00805f9b34fb"), Some("audio sink"));
    }

    #[test]
    fn class_of_device_decodes() {
        // Headset: major audio/video (0x04), minor wearable headset (0x02).
        assert_eq!(DeviceKind::of("", 0x240404 | (0x02 << 2), 0), DeviceKind::Headset);
        // Keyboard: major peripheral (0x05), minor keyboard bit.
        assert_eq!(DeviceKind::of("", (0x05 << 8) | (0x10 << 2), 0), DeviceKind::Keyboard);
        // Icon wins over an unknown class.
        assert_eq!(DeviceKind::of("phone", 0, 0), DeviceKind::Phone);
        // LE appearance: generic watch.
        assert_eq!(DeviceKind::of("", 0, 0x00c0), DeviceKind::Watch);
    }

    #[test]
    fn rssi_maps_to_bars() {
        assert_eq!(rssi_bars(-40), 4);
        assert_eq!(rssi_bars(-65), 3);
        assert_eq!(rssi_bars(-75), 2);
        assert_eq!(rssi_bars(-95), 1);
    }
}

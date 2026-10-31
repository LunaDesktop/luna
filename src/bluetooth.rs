//! Backend Bluetooth (BlueZ, D-Bus system bus).
//!
//! Warstwa jest możliwie cienka: czyta migawkę adapterów i urządzeń z
//! `org.freedesktop.DBus.ObjectManager.GetManagedObjects`, a akcje (radio,
//! skanowanie, parowanie, łączenie) wykonuje przez standardowe interfejsy
//! `org.bluez.Adapter1` i `org.bluez.Device1`.
//!
//! Parowanie wymaga agenta (`org.bluez.Agent1`), który tymczasowo rejestrujemy
//! z capability `NoInputNoOutput` — urządzenia parujemy bez pytania o PIN.

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const BLUEZ: &str = "org.bluez";
const BLUEZ_PATH: &str = "/org/bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
const DEVICE_IFACE: &str = "org.bluez.Device1";
const AGENT_MANAGER_IFACE: &str = "org.bluez.AgentManager1";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";

/// Ścieżka tymczasowego agenta parowania (właścicielem jest to połączenie).
const AGENT_PATH: &str = "/luna/bluez/agent";

/// To, co zwraca `ObjectManager.GetManagedObjects`: obiekt → interfejs → właściwości.
type ManagedObjects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

/// Adapter Bluetooth (`org.bluez.Adapter1`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adapter {
    pub path: String, // np. /org/bluez/hci0
    pub name: String, // Alias lub Name, fallback adres
    pub address: String,
    pub powered: bool,
    pub discovering: bool,
}

/// Urządzenie Bluetooth (`org.bluez.Device1`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Device {
    pub path: String,    // /org/bluez/hci0/dev_XX_XX...
    pub adapter: String, // ścieżka adaptera (org.bluez.Device1.Adapter)
    pub address: String,
    pub name: String, // Alias lub Name, fallback adres
    pub paired: bool,
    pub connected: bool,
    pub trusted: bool,
    pub rssi: Option<i16>,
    pub icon: Option<String>, // np. "audio-headphones"
}

/// Migawka stanu Bluetooth.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Czy BlueZ odpowiada.
    pub available: bool,
    pub adapters: Vec<Adapter>,
    /// Posortowane: połączone, potem sparowane, potem nazwa.
    pub devices: Vec<Device>,
    pub error: Option<String>,
}

fn system() -> Result<Connection, String> {
    Connection::system().map_err(|err| format!("BlueZ: {err}"))
}

fn as_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
}

fn as_bool(props: &HashMap<String, OwnedValue>, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v.clone()).ok())
        .unwrap_or(false)
}

fn as_i16(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i16> {
    props.get(key).and_then(|v| i16::try_from(v.clone()).ok())
}

fn as_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| OwnedObjectPath::try_from(v.clone()).ok())
        .map(|path| path.to_string())
}

/// Wybiera nazwę do wyświetlenia: `Alias` → `Name` → adres.
fn display_name(alias: Option<&str>, name: Option<&str>, address: &str) -> String {
    alias
        .filter(|value| !value.trim().is_empty())
        .or_else(|| name.filter(|value| !value.trim().is_empty()))
        .map(str::to_owned)
        .unwrap_or_else(|| address.to_owned())
}

/// BlueZ zgłasza RSSI `0`, gdy sygnał jest nieznany (urządzenie rozłączone).
fn parse_rssi(raw: i16) -> Option<i16> {
    (raw != 0).then_some(raw)
}

/// Sortuje urządzenia: połączone, potem sparowane, na końcu wg nazwy.
fn sort_devices(devices: &mut [Device]) {
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn adapter_from(path: &str, props: &HashMap<String, OwnedValue>) -> Adapter {
    let address = as_string(props, "Address").unwrap_or_default();
    let name = display_name(
        as_string(props, "Alias").as_deref(),
        as_string(props, "Name").as_deref(),
        &address,
    );
    Adapter {
        path: path.to_owned(),
        name,
        address,
        powered: as_bool(props, "Powered"),
        discovering: as_bool(props, "Discovering"),
    }
}

fn device_from(path: &str, props: &HashMap<String, OwnedValue>) -> Device {
    let address = as_string(props, "Address").unwrap_or_default();
    let name = display_name(
        as_string(props, "Alias").as_deref(),
        as_string(props, "Name").as_deref(),
        &address,
    );
    Device {
        path: path.to_owned(),
        adapter: as_path(props, "Adapter").unwrap_or_default(),
        address,
        name,
        paired: as_bool(props, "Paired"),
        connected: as_bool(props, "Connected"),
        trusted: as_bool(props, "Trusted"),
        rssi: as_i16(props, "RSSI").and_then(parse_rssi),
        icon: as_string(props, "Icon"),
    }
}

fn managed_objects(conn: &Connection) -> Result<ManagedObjects, String> {
    let reply = conn
        .call_method(
            Some(BLUEZ),
            "/",
            Some(OBJECT_MANAGER),
            "GetManagedObjects",
            &(),
        )
        .map_err(|err| format!("BlueZ: {err}"))?;
    reply
        .body()
        .deserialize::<ManagedObjects>()
        .map_err(|err| format!("BlueZ: {err}"))
}

/// Migawka adapterów i urządzeń Bluetooth.
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot::default();
    let conn = match system() {
        Ok(conn) => conn,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    let objects = match managed_objects(&conn) {
        Ok(objects) => objects,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    snap.available = true;
    for (path, interfaces) in objects {
        if let Some(props) = interfaces.get(ADAPTER_IFACE) {
            snap.adapters.push(adapter_from(path.as_str(), props));
        }
        if let Some(props) = interfaces.get(DEVICE_IFACE) {
            snap.devices.push(device_from(path.as_str(), props));
        }
    }
    sort_devices(&mut snap.devices);
    snap
}

/// Włącza/wyłącza radio Bluetooth (`Adapter1.Powered`).
pub fn set_powered(adapter: &str, powered: bool) -> Result<(), String> {
    let conn = system()?;
    conn.call_method(
        Some(BLUEZ),
        adapter,
        Some(PROPS),
        "Set",
        &(ADAPTER_IFACE, "Powered", Value::from(powered)),
    )
    .map(|_| ())
    .map_err(|err| format!("BlueZ: {err}"))
}

/// Włącza/wyłącza skanowanie (`Adapter1.StartDiscovery` / `StopDiscovery`).
pub fn set_discovering(adapter: &str, on: bool) -> Result<(), String> {
    let conn = system()?;
    let method = if on {
        "StartDiscovery"
    } else {
        "StopDiscovery"
    };
    conn.call_method(Some(BLUEZ), adapter, Some(ADAPTER_IFACE), method, &())
        .map(|_| ())
        .map_err(|err| format!("BlueZ: {err}"))
}

/// Paruje urządzenie. Rejestruje tymczasowego agenta `NoInputNoOutput`,
/// żeby BlueZ mógł potwierdzić parowanie bez pytania użytkownika.
pub fn pair(device: &str) -> Result<(), String> {
    let device = device.to_owned();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(pair_with_agent(&device));
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err("BlueZ: parowanie przekroczyło limit 60 s".to_owned())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("BlueZ: parowanie zakończyło się bez wyniku".to_owned())
        }
    }
}

fn pair_without_agent(conn: &Connection, device: &str) -> Result<(), String> {
    conn.call_method(Some(BLUEZ), device, Some(DEVICE_IFACE), "Pair", &())
        .map(|_| ())
        .map_err(|err| format!("BlueZ: {err}"))
}

fn pair_with_agent(device: &str) -> Result<(), String> {
    let conn = system()?;
    let agent_path =
        OwnedObjectPath::try_from(AGENT_PATH).map_err(|err| format!("BlueZ: {err}"))?;

    let registered = conn
        .object_server()
        .at(AGENT_PATH, Agent)
        .map_err(|err| format!("BlueZ: {err}"))
        .and_then(|_| {
            conn.call_method(
                Some(BLUEZ),
                BLUEZ_PATH,
                Some(AGENT_MANAGER_IFACE),
                "RegisterAgent",
                &(agent_path.clone(), "NoInputNoOutput"),
            )
            .map(|_| ())
            .map_err(|err| format!("BlueZ: {err}"))
        });

    match registered {
        Ok(()) => {
            let result = conn
                .call_method(Some(BLUEZ), device, Some(DEVICE_IFACE), "Pair", &())
                .map(|_| ())
                .map_err(|err| format!("BlueZ: {err}"));
            let _ = conn.call_method(
                Some(BLUEZ),
                BLUEZ_PATH,
                Some(AGENT_MANAGER_IFACE),
                "UnregisterAgent",
                &agent_path,
            );
            result
        }
        Err(agent_err) => match pair_without_agent(&conn, device) {
            Ok(()) => Ok(()),
            Err(pair_err) => Err(format!(
                "BlueZ: nie udało się zarejestrować agenta parowania ({agent_err}); \
                 parowanie bez agenta również się nie udało: {pair_err}"
            )),
        },
    }
}

/// Łączy z urządzeniem (`Device1.Connect`).
pub fn connect(device: &str) -> Result<(), String> {
    let conn = system()?;
    conn.call_method(Some(BLUEZ), device, Some(DEVICE_IFACE), "Connect", &())
        .map(|_| ())
        .map_err(|err| format!("BlueZ: {err}"))
}

/// Rozłącza urządzenie (`Device1.Disconnect`).
pub fn disconnect(device: &str) -> Result<(), String> {
    let conn = system()?;
    conn.call_method(Some(BLUEZ), device, Some(DEVICE_IFACE), "Disconnect", &())
        .map(|_| ())
        .map_err(|err| format!("BlueZ: {err}"))
}

/// Usuwa urządzenie z adaptera (`Adapter1.RemoveDevice`).
pub fn remove(adapter: &str, device: &str) -> Result<(), String> {
    let conn = system()?;
    let device = OwnedObjectPath::try_from(device).map_err(|err| format!("BlueZ: {err}"))?;
    conn.call_method(
        Some(BLUEZ),
        adapter,
        Some(ADAPTER_IFACE),
        "RemoveDevice",
        &device,
    )
    .map(|_| ())
    .map_err(|err| format!("BlueZ: {err}"))
}

/// Oznacza urządzenie jako zaufane (`Device1.Trusted`).
pub fn set_trusted(device: &str, trusted: bool) -> Result<(), String> {
    let conn = system()?;
    conn.call_method(
        Some(BLUEZ),
        device,
        Some(PROPS),
        "Set",
        &(DEVICE_IFACE, "Trusted", Value::from(trusted)),
    )
    .map(|_| ())
    .map_err(|err| format!("BlueZ: {err}"))
}

/// Minimalny agent parowania `org.bluez.Agent1` (capability `NoInputNoOutput`).
///
/// Wszystkie wywołania akceptujemy bezwarunkowo — dla `NoInputNoOutput` BlueZ
/// nie pyta o PIN ani passkey, a potwierdzenia po prostu przepuszczamy.
struct Agent;

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self) {}

    fn request_confirmation(&self, _device: OwnedObjectPath, _passkey: u32) {}

    fn request_authorization(&self, _device: OwnedObjectPath) {}

    fn authorize_service(&self, _device: OwnedObjectPath, _uuid: String) {}

    fn cancel(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_prefers_alias_then_name_then_address() {
        let address = "00:11:22:33:44:55";
        assert_eq!(display_name(Some("Alias"), Some("Name"), address), "Alias");
        assert_eq!(display_name(None, Some("Name"), address), "Name");
        assert_eq!(display_name(None, None, address), address);
        assert_eq!(display_name(Some(""), Some("Name"), address), "Name");
        assert_eq!(display_name(Some("  "), None, address), address);
    }

    #[test]
    fn rssi_maps_zero_to_none() {
        assert_eq!(parse_rssi(0), None);
        assert_eq!(parse_rssi(-42), Some(-42));
        assert_eq!(parse_rssi(127), Some(127));
        assert_eq!(parse_rssi(i16::MIN), Some(i16::MIN));
    }

    fn device(name: &str, connected: bool, paired: bool) -> Device {
        Device {
            name: name.to_owned(),
            connected,
            paired,
            ..Device::default()
        }
    }

    #[test]
    fn devices_sort_connected_then_paired_then_name() {
        let mut devices = vec![
            device("gamma", false, false),
            device("Alpha", true, false),
            device("beta", false, true),
            device("Delta", true, true),
        ];
        sort_devices(&mut devices);
        let names: Vec<&str> = devices.iter().map(|device| device.name.as_str()).collect();
        assert_eq!(names, vec!["Delta", "Alpha", "beta", "gamma"]);
    }
}

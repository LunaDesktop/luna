//! Backend NetworkManager (D-Bus, system bus).
//!
//! Warstwa jest możliwie cienka: czyta migawkę urządzeń i punktów dostępowych,
//! a akcje (połączenie, rozłączenie, skan, radio) wykonuje przez standardowe
//! metody `org.freedesktop.NetworkManager`. Hasła przekazujemy w słowniku
//! `a{sa{sv}}` — nigdy przez `argv`.

use std::collections::{HashMap, HashSet};

use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::types::{
    split_cidr, Connectivity, Device, DeviceKind, DeviceState, Ipv4Method, KnownWifi, Security,
    Snapshot, WifiNetwork, WiredConfig,
};

const NM_DEST: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const DEV_IFACE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS_IFACE: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP_IFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const CONNECTION_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const IP4_IFACE: &str = "org.freedesktop.NetworkManager.IP4Config";

type Settings = HashMap<String, HashMap<String, Value<'static>>>;
/// To, co zwraca `Settings.Connection.GetSettings`.
type ConnectionSettings = HashMap<String, HashMap<String, OwnedValue>>;

fn system() -> Result<Connection, String> {
    Connection::system().map_err(|err| format!("D-Bus (system): {err}"))
}

fn prop<T>(conn: &Connection, path: &str, iface: &str, name: &str) -> Option<T>
where
    T: TryFrom<OwnedValue>,
{
    let reply = conn
        .call_method(Some(NM_DEST), path, Some(PROPS), "Get", &(iface, name))
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    T::try_from(value).ok()
}

fn all_props(conn: &Connection, path: &str, iface: &str) -> HashMap<String, OwnedValue> {
    conn.call_method(Some(NM_DEST), path, Some(PROPS), "GetAll", &iface)
        .ok()
        .and_then(|reply| {
            reply
                .body()
                .deserialize::<HashMap<String, OwnedValue>>()
                .ok()
        })
        .unwrap_or_default()
}

fn as_u32(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
    props.get(key).and_then(|v| u32::try_from(v.clone()).ok())
}

fn as_u8(props: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
    props.get(key).and_then(|v| u8::try_from(v.clone()).ok())
}

fn as_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| String::try_from(v.clone()).ok())
}

fn as_path(props: &HashMap<String, OwnedValue>, key: &str) -> Option<OwnedObjectPath> {
    props
        .get(key)
        .and_then(|v| OwnedObjectPath::try_from(v.clone()).ok())
}

fn as_bytes(props: &HashMap<String, OwnedValue>, key: &str) -> Option<Vec<u8>> {
    props
        .get(key)
        .and_then(|v| Vec::<u8>::try_from(v.clone()).ok())
}

/// Stan radia i aktywnej sieci Wi-Fi — tanie zapytanie dla paska.
///
/// Zwraca `(czy radio włączone, SSID aktywnej sieci)`; `None`, gdy NetworkManager
/// nie odpowiada. Używane przez panel, żeby ten sam stan widziały Ustawienia.
pub fn wifi_state() -> Option<(bool, Option<String>)> {
    let conn = system().ok()?;
    let enabled = prop::<bool>(&conn, NM_PATH, NM_IFACE, "WirelessEnabled")?;
    let devices: Vec<OwnedObjectPath> = conn
        .call_method(Some(NM_DEST), NM_PATH, Some(NM_IFACE), "GetDevices", &())
        .ok()?
        .body()
        .deserialize()
        .ok()?;
    let mut ssid = None;
    for path in devices {
        let path = path.to_string();
        let props = all_props(&conn, &path, DEV_IFACE);
        if as_u32(&props, "DeviceType") != Some(2) {
            continue;
        }
        if !DeviceState::from_nm(as_u32(&props, "State").unwrap_or(0)).is_connected() {
            continue;
        }
        let Some(active) = as_path(&props, "ActiveConnection")
            .map(|path| path.to_string())
            .filter(|path| path != "/")
        else {
            continue;
        };
        let Some(ap) = prop::<OwnedObjectPath>(&conn, &active, ACTIVE_IFACE, "SpecificObject")
            .map(|path| path.to_string())
        else {
            continue;
        };
        let ap_props = all_props(&conn, &ap, AP_IFACE);
        if let Some(bytes) = as_bytes(&ap_props, "Ssid").filter(|bytes| !bytes.is_empty()) {
            ssid = Some(String::from_utf8_lossy(&bytes).into_owned());
            break;
        }
    }
    Some((enabled, ssid))
}

/// Migawka: urządzenia, Wi-Fi, łączność i stan radia.
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot::default();
    let conn = match system() {
        Ok(conn) => conn,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    let devices: Vec<OwnedObjectPath> = match conn
        .call_method(Some(NM_DEST), NM_PATH, Some(NM_IFACE), "GetDevices", &())
        .and_then(|reply| reply.body().deserialize())
    {
        Ok(devices) => devices,
        Err(err) => {
            snap.error = Some(format!("NetworkManager: {err}"));
            return snap;
        }
    };
    snap.available = true;
    let saved = saved_connections(&conn);
    snap.known_wifi = known_wifi(&saved);
    let known_ssids: HashSet<Vec<u8>> = saved
        .iter()
        .filter_map(|(_, settings)| {
            let connection = settings.get("connection")?;
            if as_string(connection, "type").as_deref() != Some("802-11-wireless") {
                return None;
            }
            as_bytes(settings.get("802-11-wireless")?, "ssid")
        })
        .collect();
    snap.wifi_enabled = prop::<bool>(&conn, NM_PATH, NM_IFACE, "WirelessEnabled").unwrap_or(false);
    snap.connectivity = conn
        .call_method(
            Some(NM_DEST),
            NM_PATH,
            Some(NM_IFACE),
            "CheckConnectivity",
            &(),
        )
        .ok()
        .and_then(|reply| reply.body().deserialize::<u32>().ok())
        .map(Connectivity::from_nm)
        .unwrap_or_default();

    let mut wifi: HashMap<String, WifiNetwork> = HashMap::new();
    for path in devices {
        let path = path.to_string();
        let props = all_props(&conn, &path, DEV_IFACE);
        let kind = match as_u32(&props, "DeviceType").unwrap_or(0) {
            1 => DeviceKind::Ethernet,
            2 => DeviceKind::Wifi,
            _ => DeviceKind::Other,
        };
        if kind == DeviceKind::Other {
            continue;
        }
        let interface = as_string(&props, "Interface").unwrap_or_default();
        let state = DeviceState::from_nm(as_u32(&props, "State").unwrap_or(0));
        let active_path = as_path(&props, "ActiveConnection")
            .map(|path| path.to_string())
            .filter(|path| path != "/");
        let connection = active_path
            .as_deref()
            .and_then(|path| prop::<String>(&conn, path, ACTIVE_IFACE, "Id"));
        let ip4 = as_path(&props, "Ip4Config")
            .map(|path| path.to_string())
            .filter(|path| path != "/")
            .and_then(|path| ip4_address(&conn, &path));
        let metered = matches!(as_u32(&props, "Metered").unwrap_or(0), 1 | 3);
        let wired = wired_config_for(&conn, &saved, active_path.as_deref(), kind);
        snap.devices.push(Device {
            path: path.clone(),
            interface: interface.clone(),
            kind,
            state,
            driver: as_string(&props, "Driver").unwrap_or_default(),
            hw_address: as_string(&props, "HwAddress").unwrap_or_default(),
            connection,
            ip4,
            wired,
            metered,
        });

        if kind == DeviceKind::Wifi {
            let active_ap = active_path
                .as_deref()
                .and_then(|path| {
                    prop::<OwnedObjectPath>(&conn, path, ACTIVE_IFACE, "SpecificObject")
                })
                .map(|path| path.to_string());
            let aps: Vec<OwnedObjectPath> = conn
                .call_method(
                    Some(NM_DEST),
                    path.as_str(),
                    Some(WIRELESS_IFACE),
                    "GetAccessPoints",
                    &(),
                )
                .ok()
                .and_then(|reply| reply.body().deserialize().ok())
                .unwrap_or_default();
            for ap in aps {
                let ap_path = ap.to_string();
                let ap_props = all_props(&conn, &ap_path, AP_IFACE);
                let ssid = as_bytes(&ap_props, "Ssid")
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default();
                if ssid.is_empty() {
                    continue;
                }
                let security = Security::from_flags(
                    as_u32(&ap_props, "Flags").unwrap_or(0),
                    as_u32(&ap_props, "WpaFlags").unwrap_or(0),
                    as_u32(&ap_props, "RsnFlags").unwrap_or(0),
                );
                let active = active_ap.as_deref() == Some(ap_path.as_str());
                let network = WifiNetwork {
                    ssid: ssid.clone(),
                    signal: as_u8(&ap_props, "Strength").unwrap_or(0),
                    security,
                    active,
                    device: path.clone(),
                    device_name: interface.clone(),
                    ap_path,
                    frequency: as_u32(&ap_props, "Frequency").unwrap_or(0),
                    known: known_ssids.contains(ssid.as_bytes()),
                };
                wifi.entry(ssid)
                    .and_modify(|existing| {
                        if network.active || (!existing.active && network.signal > existing.signal)
                        {
                            *existing = network.clone();
                        }
                    })
                    .or_insert(network);
            }
        }
    }
    snap.wifi = wifi.into_values().collect();
    snap.wifi.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });
    snap
}

fn ip4_address(conn: &Connection, path: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some(NM_DEST),
            path,
            Some(PROPS),
            "Get",
            &(IP4_IFACE, "AddressData"),
        )
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    let entries: Vec<HashMap<String, OwnedValue>> = Vec::try_from(value).ok()?;
    let first = entries.first()?;
    let address = first
        .get("address")
        .and_then(|value| String::try_from(value.clone()).ok())?;
    let prefix = first
        .get("prefix")
        .and_then(|value| u32::try_from(value.clone()).ok())
        .unwrap_or(24);
    Some(format!("{address}/{prefix}"))
}

/// Włącza/wyłącza radio Wi-Fi (`NM.WirelessEnabled`).
pub fn set_wifi_enabled(enabled: bool) -> Result<(), String> {
    let conn = system()?;
    conn.call_method(
        Some(NM_DEST),
        NM_PATH,
        Some(PROPS),
        "Set",
        &(NM_IFACE, "WirelessEnabled", Value::from(enabled)),
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

/// Prosi wszystkie urządzenia Wi-Fi o świeży skan.
pub fn request_wifi_scan() -> Result<(), String> {
    let conn = system()?;
    let devices: Vec<OwnedObjectPath> = conn
        .call_method(Some(NM_DEST), NM_PATH, Some(NM_IFACE), "GetDevices", &())
        .map_err(|err| format!("NetworkManager: {err}"))?
        .body()
        .deserialize()
        .map_err(|err| format!("NetworkManager: {err}"))?;
    let options: HashMap<String, Value<'_>> = HashMap::new();
    for path in devices {
        let path = path.to_string();
        let props = all_props(&conn, &path, DEV_IFACE);
        if as_u32(&props, "DeviceType") != Some(2) {
            continue;
        }
        let _ = conn.call_method(
            Some(NM_DEST),
            path.as_str(),
            Some(WIRELESS_IFACE),
            "RequestScan",
            &options,
        );
    }
    Ok(())
}

/// Łączy z siecią Wi-Fi na wskazanym urządzeniu.
pub fn connect_wifi(
    device_path: &str,
    ssid: &str,
    security: Security,
    password: Option<&str>,
    hidden: bool,
) -> Result<(), String> {
    if !security.is_supported() {
        return Err(format!(
            "{} networks are not supported yet — use NetworkManager directly",
            security.label()
        ));
    }
    let password = password.filter(|value| !value.is_empty());
    if security.needs_password() && password.is_none() {
        return Err("A password is required".to_owned());
    }
    let conn = system()?;
    if let Some((path, uuid, id)) = find_wifi_connection(&conn, ssid) {
        let settings = wifi_settings(ssid, security, password, hidden, Some((&uuid, &id)))?;
        update_connection(&conn, path.as_str(), &settings)?;
        activate_existing(&conn, path.as_str(), device_path)
    } else {
        let settings = wifi_settings(ssid, security, password, hidden, None)?;
        add_and_activate(&conn, &settings, device_path)
    }
}

/// Łączy interfejs przewodowy (profil DHCP tworzony automatycznie).
pub fn connect_ethernet(device_path: &str) -> Result<(), String> {
    let conn = system()?;
    if let Some((path, _, _)) = find_wired_connection(&conn) {
        return activate_existing(&conn, path.as_str(), device_path);
    }
    let settings = wired_settings(
        &new_uuid(),
        "Wired connection",
        &WiredConfig {
            autoconnect: true,
            ..WiredConfig::default()
        },
    );
    add_and_activate(&conn, &settings, device_path)
}

/// Rozłącza urządzenie (nie usuwa zapisanego profilu).
pub fn disconnect_device(device_path: &str) -> Result<(), String> {
    let conn = system()?;
    conn.call_method(
        Some(NM_DEST),
        device_path,
        Some(DEV_IFACE),
        "Disconnect",
        &(),
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

/// Zapisuje IPv4 profilu przewodowego i od razu go aktywuje, jeśli kabel jest podłączony.
///
/// Tworzenie profilu przy aktywnym urządzeniu idzie przez `AddAndActivateConnection`,
/// bo ta ścieżka wymaga tylko `network-control` (bez hasła administratora).
pub fn set_wired_ipv4(device_path: &str, config: &WiredConfig) -> Result<(), String> {
    config.validate().map_err(|err| err.to_string())?;
    let conn = system()?;
    let existing = match config.profile.as_deref().filter(|path| !path.is_empty()) {
        Some(path) => Some(path.to_owned()),
        None => find_wired_connection(&conn).map(|(path, _, _)| path.to_string()),
    };
    let state = prop::<u32>(&conn, device_path, DEV_IFACE, "State").unwrap_or(0);
    let active = matches!(state, 70 | 80 | 90 | 100);
    if let Some(path) = existing {
        let current = connection_settings(&conn, &path)
            .ok_or_else(|| format!("NetworkManager: cannot read profile {path}"))?;
        let connection = current.get("connection");
        let uuid = connection
            .and_then(|value| as_string(value, "uuid"))
            .unwrap_or_else(new_uuid);
        let id = connection
            .and_then(|value| as_string(value, "id"))
            .unwrap_or_else(|| "Wired connection".to_owned());
        let settings = wired_settings(&uuid, &id, config);
        update_connection(&conn, &path, &settings)?;
        if active {
            activate_existing(&conn, &path, device_path)?;
        }
        return Ok(());
    }
    let settings = wired_settings(&new_uuid(), "Wired connection", config);
    if active {
        return add_and_activate_path(&conn, &settings, device_path).map(|_| ());
    }
    let reply = conn
        .call_method(
            Some(NM_DEST),
            SETTINGS_PATH,
            Some(SETTINGS_IFACE),
            "AddConnection",
            &settings,
        )
        .map_err(|err| format!("NetworkManager: {err}"))?;
    reply
        .body()
        .deserialize::<OwnedObjectPath>()
        .map(|_| ())
        .map_err(|err| format!("NetworkManager: {err}"))
}

/// Włącza/wyłącza automatyczne łączenie z zapisanym profilem (`Update` scala ustawienia).
pub fn set_autoconnect(profile_path: &str, enabled: bool) -> Result<(), String> {
    let conn = system()?;
    let mut connection = HashMap::new();
    connection.insert("autoconnect".to_owned(), Value::from(enabled));
    let mut settings = HashMap::new();
    settings.insert("connection".to_owned(), connection);
    update_connection(&conn, profile_path, &settings)
}

/// Usuwa zapisany profil Wi-Fi o danym SSID.
pub fn forget_wifi(ssid: &str) -> Result<bool, String> {
    let conn = system()?;
    let Some((path, _, _)) = find_wifi_connection(&conn, ssid) else {
        return Ok(false);
    };
    forget_connection(path.as_str())?;
    Ok(true)
}

/// Usuwa dowolny zapisany profil (Wi-Fi, przewodowy).
pub fn forget_connection(profile_path: &str) -> Result<(), String> {
    let conn = system()?;
    conn.call_method(
        Some(NM_DEST),
        profile_path,
        Some(CONNECTION_IFACE),
        "Delete",
        &(),
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

fn saved_connections(conn: &Connection) -> Vec<(String, ConnectionSettings)> {
    let Ok(reply) = conn.call_method(
        Some(NM_DEST),
        SETTINGS_PATH,
        Some(SETTINGS_IFACE),
        "ListConnections",
        &(),
    ) else {
        return Vec::new();
    };
    let Ok(paths) = reply.body().deserialize::<Vec<OwnedObjectPath>>() else {
        return Vec::new();
    };
    paths
        .into_iter()
        .filter_map(|path| {
            let path = path.to_string();
            let settings = connection_settings(conn, &path)?;
            Some((path, settings))
        })
        .collect()
}

fn known_wifi(saved: &[(String, ConnectionSettings)]) -> Vec<KnownWifi> {
    let mut known: Vec<KnownWifi> = saved
        .iter()
        .filter_map(|(profile, settings)| {
            let connection = settings.get("connection")?;
            if as_string(connection, "type").as_deref() != Some("802-11-wireless") {
                return None;
            }
            let ssid = as_bytes(settings.get("802-11-wireless")?, "ssid")?;
            if ssid.is_empty() {
                return None;
            }
            let autoconnect = connection
                .get("autoconnect")
                .and_then(|value| bool::try_from(value.clone()).ok())
                .unwrap_or(true);
            Some(KnownWifi {
                ssid: String::from_utf8_lossy(&ssid).into_owned(),
                profile: profile.clone(),
                autoconnect,
            })
        })
        .collect();
    known.sort_by_key(|known| known.ssid.to_lowercase());
    known
}

fn find_wifi_connection(
    conn: &Connection,
    ssid: &str,
) -> Option<(OwnedObjectPath, String, String)> {
    for (path, settings) in saved_connections(conn) {
        let Some(connection) = settings.get("connection") else {
            continue;
        };
        if as_string(connection, "type").as_deref() != Some("802-11-wireless") {
            continue;
        }
        let Some(candidate) = settings
            .get("802-11-wireless")
            .and_then(|wireless| as_bytes(wireless, "ssid"))
        else {
            continue;
        };
        if candidate != ssid.as_bytes() {
            continue;
        }
        let uuid = as_string(connection, "uuid").unwrap_or_default();
        let id = as_string(connection, "id").unwrap_or_else(|| ssid.to_owned());
        return Some((OwnedObjectPath::try_from(path).ok()?, uuid, id));
    }
    None
}

fn find_wired_connection(conn: &Connection) -> Option<(OwnedObjectPath, String, String)> {
    for (path, settings) in saved_connections(conn) {
        let Some(connection) = settings.get("connection") else {
            continue;
        };
        if as_string(connection, "type").as_deref() != Some("802-3-ethernet") {
            continue;
        }
        let uuid = as_string(connection, "uuid").unwrap_or_default();
        let id = as_string(connection, "id").unwrap_or_default();
        return Some((OwnedObjectPath::try_from(path).ok()?, uuid, id));
    }
    None
}

/// Czyta IPv4 z profilu powiązanego z urządzeniem przewodowym.
fn wired_config_for(
    conn: &Connection,
    saved: &[(String, ConnectionSettings)],
    active: Option<&str>,
    kind: DeviceKind,
) -> Option<WiredConfig> {
    if kind != DeviceKind::Ethernet {
        return None;
    }
    let profile = active
        .and_then(|path| prop::<OwnedObjectPath>(conn, path, ACTIVE_IFACE, "Connection"))
        .map(|path| path.to_string())
        .or_else(|| find_wired_connection(conn).map(|(path, _, _)| path.to_string()))?;
    let settings = saved
        .iter()
        .find(|(saved_path, _)| *saved_path == profile)
        .map(|(_, settings)| settings)?;
    Some(wired_config_from(&profile, settings))
}

fn wired_config_from(profile: &str, settings: &ConnectionSettings) -> WiredConfig {
    let ipv4 = settings.get("ipv4");
    let method = ipv4
        .and_then(|ipv4| as_string(ipv4, "method"))
        .map(|method| Ipv4Method::from_nm(&method))
        .unwrap_or_default();
    let mut config = WiredConfig {
        method,
        address: String::new(),
        gateway: ipv4
            .and_then(|ipv4| as_string(ipv4, "gateway"))
            .unwrap_or_default(),
        dns: ipv4.map(dns_list).unwrap_or_default(),
        profile: Some(profile.to_owned()),
        autoconnect: settings
            .get("connection")
            .and_then(|connection| connection.get("autoconnect"))
            .and_then(|value| bool::try_from(value.clone()).ok())
            .unwrap_or(true),
    };
    if config.method == Ipv4Method::Manual {
        config.address = ipv4.and_then(address_list).unwrap_or_default();
    }
    config
}

fn address_list(ipv4: &HashMap<String, OwnedValue>) -> Option<String> {
    if let Some(value) = ipv4.get("address-data") {
        let entries: Vec<HashMap<String, OwnedValue>> = Vec::try_from(value.clone()).ok()?;
        let first = entries.first()?;
        let address = first
            .get("address")
            .and_then(|value| String::try_from(value.clone()).ok())?;
        let prefix = first
            .get("prefix")
            .and_then(|value| u32::try_from(value.clone()).ok())
            .unwrap_or(24);
        return Some(format!("{address}/{prefix}"));
    }
    // Stary format `addresses`: tablice `[adres, prefix, brama]` jako u32.
    let entries: Vec<Vec<u32>> = Vec::try_from(ipv4.get("addresses")?.clone()).ok()?;
    let entry = entries.first()?;
    let address = std::net::Ipv4Addr::from(entry.first().copied()?);
    let prefix = entry.get(1).copied().unwrap_or(24);
    Some(format!("{address}/{prefix}"))
}

fn dns_list(ipv4: &HashMap<String, OwnedValue>) -> Vec<String> {
    if let Some(value) = ipv4.get("dns-data") {
        if let Ok(entries) = Vec::<String>::try_from(value.clone()) {
            return entries;
        }
        let Ok(entries) = Vec::<HashMap<String, OwnedValue>>::try_from(value.clone()) else {
            return Vec::new();
        };
        return entries
            .into_iter()
            .filter_map(|entry| {
                entry
                    .get("address")
                    .and_then(|value| String::try_from(value.clone()).ok())
            })
            .collect();
    }
    ipv4.get("dns")
        .and_then(|value| Vec::<u32>::try_from(value.clone()).ok())
        .map(|list| {
            list.into_iter()
                .map(|ip| std::net::Ipv4Addr::from(ip).to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn connection_settings(conn: &Connection, path: &str) -> Option<ConnectionSettings> {
    conn.call_method(
        Some(NM_DEST),
        path,
        Some(CONNECTION_IFACE),
        "GetSettings",
        &(),
    )
    .ok()?
    .body()
    .deserialize()
    .ok()
}

fn wifi_settings(
    ssid: &str,
    security: Security,
    password: Option<&str>,
    hidden: bool,
    existing: Option<(&str, &str)>,
) -> Result<Settings, String> {
    let (uuid, id) = match existing {
        Some((uuid, id)) if !uuid.is_empty() => (uuid.to_owned(), id.to_owned()),
        _ => (new_uuid(), ssid.to_owned()),
    };
    let mut connection = HashMap::new();
    connection.insert("id".to_owned(), Value::from(id));
    connection.insert("type".to_owned(), Value::from("802-11-wireless".to_owned()));
    connection.insert("uuid".to_owned(), Value::from(uuid));
    connection.insert("autoconnect".to_owned(), Value::from(true));

    let mut wireless = HashMap::new();
    wireless.insert("ssid".to_owned(), Value::from(ssid.as_bytes().to_vec()));
    wireless.insert("mode".to_owned(), Value::from("infrastructure".to_owned()));
    if hidden {
        wireless.insert("hidden".to_owned(), Value::from(true));
    }

    let mut settings = HashMap::new();
    settings.insert("connection".to_owned(), connection);
    settings.insert("802-11-wireless".to_owned(), wireless);
    if let Some(key_mgmt) = security.key_mgmt() {
        let mut sec = HashMap::new();
        sec.insert("key-mgmt".to_owned(), Value::from(key_mgmt.to_owned()));
        if let Some(password) = password {
            sec.insert("psk".to_owned(), Value::from(password.to_owned()));
        }
        settings.insert("802-11-wireless-security".to_owned(), sec);
    }
    settings.insert("ipv4".to_owned(), ipv4_auto());
    settings.insert("ipv6".to_owned(), ipv6_auto());
    Ok(settings)
}

fn wired_settings(uuid: &str, id: &str, config: &WiredConfig) -> Settings {
    let mut connection = HashMap::new();
    connection.insert("id".to_owned(), Value::from(id.to_owned()));
    connection.insert("type".to_owned(), Value::from("802-3-ethernet".to_owned()));
    connection.insert("uuid".to_owned(), Value::from(uuid.to_owned()));
    connection.insert("autoconnect".to_owned(), Value::from(config.autoconnect));
    let mut settings = HashMap::new();
    settings.insert("connection".to_owned(), connection);
    settings.insert("ipv4".to_owned(), ipv4_settings(config));
    settings.insert("ipv6".to_owned(), ipv6_auto());
    settings
}

/// Sekcja `ipv4` profilu: DHCP (`auto`) albo adres/brama/DNS (`manual`).
fn ipv4_settings(config: &WiredConfig) -> HashMap<String, Value<'static>> {
    let mut map = HashMap::new();
    map.insert(
        "method".to_owned(),
        Value::from(config.method.as_nm().to_owned()),
    );
    if config.method != Ipv4Method::Manual {
        return map;
    }
    if let Some((address, prefix)) = split_cidr(&config.address) {
        let mut entry = HashMap::new();
        entry.insert("address".to_owned(), Value::from(address.to_string()));
        entry.insert("prefix".to_owned(), Value::from(u32::from(prefix)));
        map.insert("address-data".to_owned(), Value::from(vec![entry]));
    }
    let gateway = config.gateway.trim();
    if !gateway.is_empty() {
        map.insert("gateway".to_owned(), Value::from(gateway.to_owned()));
    }
    let dns: Vec<std::net::Ipv4Addr> = config
        .dns
        .iter()
        .filter_map(|dns| dns.trim().parse().ok())
        .collect();
    if !dns.is_empty() {
        // `dns-data` (od NM 1.54) to tablica adresów w postaci tekstowej.
        map.insert(
            "dns-data".to_owned(),
            Value::from(
                dns.iter()
                    .map(|address| address.to_string())
                    .collect::<Vec<String>>(),
            ),
        );
        // Starsze wersje czytają wyłącznie `dns` (tablica u32).
        map.insert(
            "dns".to_owned(),
            Value::from(
                dns.iter()
                    .map(|address| u32::from(*address))
                    .collect::<Vec<u32>>(),
            ),
        );
    }
    map
}

fn ipv4_auto() -> HashMap<String, Value<'static>> {
    let mut map = HashMap::new();
    map.insert("method".to_owned(), Value::from("auto".to_owned()));
    map
}

fn ipv6_auto() -> HashMap<String, Value<'static>> {
    let mut map = HashMap::new();
    map.insert("method".to_owned(), Value::from("auto".to_owned()));
    map
}

fn update_connection(conn: &Connection, path: &str, settings: &Settings) -> Result<(), String> {
    conn.call_method(
        Some(NM_DEST),
        path,
        Some(CONNECTION_IFACE),
        "Update",
        &settings,
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

fn activate_existing(conn: &Connection, path: &str, device: &str) -> Result<(), String> {
    let root = ObjectPath::from_static_str("/").expect("root path");
    conn.call_method(
        Some(NM_DEST),
        NM_PATH,
        Some(NM_IFACE),
        "ActivateConnection",
        &(path, device, root),
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

fn add_and_activate(conn: &Connection, settings: &Settings, device: &str) -> Result<(), String> {
    add_and_activate_path(conn, settings, device).map(|_| ())
}

/// `AddAndActivateConnection` zwraca parę `(profil, aktywne połączenie)`.
fn add_and_activate_path(
    conn: &Connection,
    settings: &Settings,
    device: &str,
) -> Result<String, String> {
    let root = ObjectPath::from_static_str("/").expect("root path");
    let reply = conn
        .call_method(
            Some(NM_DEST),
            NM_PATH,
            Some(NM_IFACE),
            "AddAndActivateConnection",
            &(settings, root, device),
        )
        .map_err(|err| format!("NetworkManager: {err}"))?;
    let (path, _active): (OwnedObjectPath, OwnedObjectPath) = reply
        .body()
        .deserialize()
        .map_err(|err| format!("NetworkManager: {err}"))?;
    Ok(path.to_string())
}

/// UUID v4-podobny bez zależności (wystarczający dla profilu NM).
fn new_uuid() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let salt = (std::process::id() as u128) << 64;
    let value = nanos ^ salt ^ 0x9e37_79b9_7f4a_7c15_9e37_79b9_7f4a_7c15;
    let hex = format!("{value:032x}");
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value_string(map: &HashMap<String, Value<'_>>, key: &str) -> Option<String> {
        map.get(key)
            .and_then(|value| String::try_from(value.clone()).ok())
    }

    fn value_bytes(map: &HashMap<String, Value<'_>>, key: &str) -> Option<Vec<u8>> {
        map.get(key)
            .and_then(|value| Vec::<u8>::try_from(value.clone()).ok())
    }

    #[test]
    fn uuid_has_expected_shape() {
        let uuid = new_uuid();
        assert_eq!(uuid.len(), 36);
        assert_eq!(uuid.chars().filter(|ch| *ch == '-').count(), 4);
    }

    #[test]
    fn wifi_settings_contains_ssid_and_secret() {
        let settings =
            wifi_settings("Dom", Security::Wpa2, Some("tajne"), false, None).expect("settings");
        let ssid =
            value_bytes(settings.get("802-11-wireless").expect("wireless"), "ssid").expect("ssid");
        assert_eq!(ssid, b"Dom");
        let security = settings.get("802-11-wireless-security").expect("security");
        assert_eq!(
            value_string(security, "key-mgmt").as_deref(),
            Some("wpa-psk")
        );
        assert_eq!(value_string(security, "psk").as_deref(), Some("tajne"));
    }

    #[test]
    fn open_network_has_no_security_section() {
        let settings = wifi_settings("Kawiarnia", Security::Open, None, false, None).expect("ok");
        assert!(!settings.contains_key("802-11-wireless-security"));
    }

    fn manual_config() -> WiredConfig {
        WiredConfig {
            method: Ipv4Method::Manual,
            address: "192.168.1.20/24".to_owned(),
            gateway: "192.168.1.1".to_owned(),
            dns: vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()],
            profile: None,
            autoconnect: true,
        }
    }

    fn owned(settings: Settings) -> HashMap<String, HashMap<String, OwnedValue>> {
        settings
            .into_iter()
            .map(|(section, values)| {
                let values = values
                    .into_iter()
                    .map(|(key, value)| (key, OwnedValue::try_from(value).expect("owned")))
                    .collect();
                (section, values)
            })
            .collect()
    }

    #[test]
    fn dhcp_wired_settings_only_set_the_method() {
        let settings = wired_settings(&new_uuid(), "Wired connection", &WiredConfig::default());
        let ipv4 = settings.get("ipv4").expect("ipv4");
        assert_eq!(value_string(ipv4, "method").as_deref(), Some("auto"));
        assert!(!ipv4.contains_key("address-data"));
        assert!(!ipv4.contains_key("dns-data"));
    }

    #[test]
    fn manual_ipv4_settings_carry_address_gateway_and_dns() {
        let ipv4 = ipv4_settings(&manual_config());
        assert_eq!(value_string(&ipv4, "method").as_deref(), Some("manual"));
        assert_eq!(
            value_string(&ipv4, "gateway").as_deref(),
            Some("192.168.1.1")
        );
        let entries: Vec<HashMap<String, Value<'_>>> =
            Vec::try_from(ipv4.get("address-data").expect("addresses").clone()).expect("entries");
        let first = entries.first().expect("first address");
        assert_eq!(
            value_string(first, "address").as_deref(),
            Some("192.168.1.20")
        );
        assert_eq!(
            first
                .get("prefix")
                .and_then(|value| u32::try_from(value.clone()).ok()),
            Some(24)
        );
        let dns: Vec<String> =
            Vec::try_from(ipv4.get("dns-data").expect("dns").clone()).expect("dns entries");
        assert_eq!(dns, vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()]);
    }

    #[test]
    fn wired_config_from_parses_manual_profile() {
        let settings = wired_settings("uuid", "Wired connection", &manual_config());
        let parsed = wired_config_from("/profile", &owned(settings));
        assert_eq!(parsed.method, Ipv4Method::Manual);
        assert_eq!(parsed.address, "192.168.1.20/24");
        assert_eq!(parsed.gateway, "192.168.1.1");
        assert_eq!(parsed.dns, vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()]);
        assert_eq!(parsed.profile.as_deref(), Some("/profile"));
        assert!(parsed.autoconnect);
    }

    #[test]
    fn wired_config_from_parses_legacy_addresses() {
        let mut ipv4 = HashMap::new();
        ipv4.insert("method".to_owned(), Value::from("manual".to_owned()));
        ipv4.insert(
            "addresses".to_owned(),
            Value::from(vec![vec![
                u32::from(std::net::Ipv4Addr::new(10, 0, 0, 7)),
                8,
                0,
            ]]),
        );
        ipv4.insert(
            "dns".to_owned(),
            Value::from(vec![u32::from(std::net::Ipv4Addr::new(9, 9, 9, 9))]),
        );
        let mut connection = HashMap::new();
        connection.insert("autoconnect".to_owned(), Value::from(false));
        let mut settings = HashMap::new();
        settings.insert("ipv4".to_owned(), ipv4);
        settings.insert("connection".to_owned(), connection);
        let parsed = wired_config_from("/legacy", &owned(settings));
        assert_eq!(parsed.address, "10.0.0.7/8");
        assert_eq!(parsed.dns, vec!["9.9.9.9".to_owned()]);
        assert!(!parsed.autoconnect);
    }

    /// Test na żywej magistrali systemowej (NetworkManager). Pomijany domyślnie.
    ///
    /// Dodaje profil bez przypięcia do urządzenia i od razu go usuwa, więc nie
    /// zmienia konfiguracji sieci. Na systemach z `auth_admin_keep` dla
    /// `settings.modify.system` uruchom go z uprawnieniami administratora.
    #[test]
    #[ignore = "wymaga NetworkManagera i uprawnień do profili systemowych"]
    fn live_wired_profile_round_trip() {
        let conn = system().expect("system bus");
        let config = WiredConfig {
            autoconnect: false,
            ..manual_config()
        };
        let settings = wired_settings(&new_uuid(), "luna-settings test", &config);
        let reply = conn
            .call_method(
                Some(NM_DEST),
                SETTINGS_PATH,
                Some(SETTINGS_IFACE),
                "AddConnection",
                &settings,
            )
            .expect("add connection");
        let path: OwnedObjectPath = reply.body().deserialize().expect("path");
        let back = connection_settings(&conn, path.as_str()).expect("get settings");
        let parsed = wired_config_from(path.as_str(), &back);
        let _ = forget_connection(path.as_str());
        assert_eq!(parsed.method, Ipv4Method::Manual);
        assert_eq!(parsed.address, "192.168.1.20/24");
        assert_eq!(parsed.gateway, "192.168.1.1");
        assert_eq!(parsed.dns.len(), 2);
        assert!(!parsed.autoconnect);
    }

    /// Test na żywej magistrali systemowej (NetworkManager). Pomijany domyślnie.
    #[test]
    #[ignore = "wymaga działającego NetworkManagera"]
    fn snapshot_reads_live_network_manager() {
        let snapshot = snapshot();
        assert!(snapshot.available, "{:?}", snapshot.error);
        assert!(!snapshot.devices.is_empty());
        assert!(snapshot
            .devices
            .iter()
            .any(|device| device.kind != DeviceKind::Other));
        if std::env::var_os("SNAPSHOT_DEBUG").is_some() {
            eprintln!("devices: {:#?}", snapshot.devices);
            eprintln!("wifi: {:#?}", snapshot.wifi);
        }
    }
}

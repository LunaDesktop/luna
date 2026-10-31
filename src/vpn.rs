//! Profile VPN NetworkManagera (D-Bus, magistrala systemowa).
//!
//! Lista profili, łączenie/rozłączanie i usuwanie idą przez standardowe metody
//! `org.freedesktop.NetworkManager`. Jedyny wyjątek to [`import`]: NetworkManager
//! nie ma metody D-Bus do importowania plików `.ovpn`/`.conf`/`.wg`, więc ten
//! moduł woła własny frontend CLI (`nmcli connection import`). To jedyne miejsce
//! zależne od `nmcli` — cała reszta warstwy VPN korzysta z magistrali.

use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};

use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue};

const NM_DEST: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_IFACE: &str = "org.freedesktop.NetworkManager";
const PROPS: &str = "org.freedesktop.DBus.Properties";
const ACTIVE_IFACE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const SETTINGS_IFACE: &str = "org.freedesktop.NetworkManager.Settings";
const SETTINGS_PATH: &str = "/org/freedesktop/NetworkManager/Settings";
const CONNECTION_IFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";

/// To, co zwraca `Settings.Connection.GetSettings` (`a{sa{sv}}`).
type ConnectionSettings = HashMap<String, HashMap<String, OwnedValue>>;

/// Zapisany profil VPN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpnProfile {
    pub name: String,
    pub uuid: String,
    /// `/org/freedesktop/NetworkManager/Settings/N`
    pub path: String,
    /// `"vpn"` albo `"wireguard"` (wartość `connection.type`).
    pub kind: String,
    pub active: bool,
}

/// Migawka profili VPN dla UI.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// Czy NetworkManager odpowiedział.
    pub available: bool,
    pub profiles: Vec<VpnProfile>,
    pub error: Option<String>,
}

/// Lista profili VPN (aktywne najpierw, potem alfabetycznie po nazwie).
pub fn snapshot() -> Snapshot {
    let mut snap = Snapshot::default();
    let conn = match system() {
        Ok(conn) => conn,
        Err(err) => {
            snap.error = Some(err);
            return snap;
        }
    };
    snap.available = true;
    let saved = saved_connections(&conn);
    let active = active_uuids(&conn);
    snap.profiles = profiles_from(&saved, &active);
    snap
}

/// Aktywuje profil VPN o danym UUID (urządzenie wybiera NetworkManager).
pub fn connect(uuid: &str) -> Result<(), String> {
    let conn = system()?;
    let Some(path) = find_profile(&conn, uuid) else {
        return Err(format!("VPN profile not found: {uuid}"));
    };
    let root = ObjectPath::from_static_str("/").expect("root path");
    let profile =
        ObjectPath::try_from(path.as_str()).map_err(|err| format!("NetworkManager: {err}"))?;
    conn.call_method(
        Some(NM_DEST),
        NM_PATH,
        Some(NM_IFACE),
        "ActivateConnection",
        &(profile, root.clone(), root),
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

/// Rozłącza aktywne połączenie VPN o danym UUID.
pub fn disconnect(uuid: &str) -> Result<(), String> {
    let conn = system()?;
    let Some(active_path) = active_connection_path(&conn, uuid) else {
        return Err(format!("VPN is not active: {uuid}"));
    };
    conn.call_method(
        Some(NM_DEST),
        active_path.as_str(),
        Some(ACTIVE_IFACE),
        "Deactivate",
        &(),
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

/// Usuwa zapisany profil VPN o danym UUID.
pub fn remove(uuid: &str) -> Result<(), String> {
    let conn = system()?;
    let Some(path) = find_profile(&conn, uuid) else {
        return Err(format!("VPN profile not found: {uuid}"));
    };
    conn.call_method(
        Some(NM_DEST),
        path.as_str(),
        Some(CONNECTION_IFACE),
        "Delete",
        &(),
    )
    .map(|_| ())
    .map_err(|err| format!("NetworkManager: {err}"))
}

/// Importuje plik VPN przez `nmcli connection import`.
///
/// NetworkManager nie udostępnia metody D-Bus do importu plików, więc używamy
/// jego własnego frontendu CLI. To jedyne miejsce w module zależne od `nmcli`;
/// typ (`openvpn`/`wireguard`) wybieramy po rozszerzeniu, a dla `.conf` po treści.
pub fn import(path: &str) -> Result<(), String> {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    let extension = extension.as_deref().unwrap_or("");
    let contents = if extension == "conf" {
        std::fs::read_to_string(path).ok()
    } else {
        None
    };
    let kind = import_kind(extension, contents.as_deref())
        .ok_or_else(|| format!("Unsupported VPN file: {path}"))?;

    let output = Command::new("nmcli")
        .args(["connection", "import", "type", kind, "file", path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("nmcli: {err}"))?;

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .unwrap_or("nmcli: import failed")
        .to_owned())
}

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

fn as_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|value| String::try_from(value.clone()).ok())
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

/// UUID aktywnie podłączonych połączeń (`NM.ActiveConnections` → `Active.Uuid`).
fn active_uuids(conn: &Connection) -> HashSet<String> {
    let Ok(reply) = conn.call_method(
        Some(NM_DEST),
        NM_PATH,
        Some(NM_IFACE),
        "ActiveConnections",
        &(),
    ) else {
        return HashSet::new();
    };
    let Ok(paths) = reply.body().deserialize::<Vec<OwnedObjectPath>>() else {
        return HashSet::new();
    };
    paths
        .into_iter()
        .filter_map(|path| prop::<String>(conn, &path.to_string(), ACTIVE_IFACE, "Uuid"))
        .collect()
}

/// Buduje posortowaną listę profili VPN z zapisanych połączeń i aktywnych UUID.
fn profiles_from(
    saved: &[(String, ConnectionSettings)],
    active_uuids: &HashSet<String>,
) -> Vec<VpnProfile> {
    let mut profiles: Vec<VpnProfile> = saved
        .iter()
        .filter_map(|(path, settings)| {
            let connection = settings.get("connection")?;
            let kind = as_string(connection, "type")?;
            if kind != "vpn" && kind != "wireguard" {
                return None;
            }
            let name = as_string(connection, "id").unwrap_or_default();
            let uuid = as_string(connection, "uuid").unwrap_or_default();
            let active = active_uuids.contains(&uuid);
            Some(VpnProfile {
                name,
                uuid,
                path: path.clone(),
                kind,
                active,
            })
        })
        .collect();
    sort_profiles(&mut profiles);
    profiles
}

/// Aktywne najpierw, potem po nazwie (bez rozróżniania wielkości liter).
fn sort_profiles(profiles: &mut [VpnProfile]) {
    profiles.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
}

fn find_profile(conn: &Connection, uuid: &str) -> Option<String> {
    saved_connections(conn)
        .into_iter()
        .find_map(|(path, settings)| {
            let matches = settings
                .get("connection")
                .and_then(|connection| as_string(connection, "uuid"))
                .as_deref()
                == Some(uuid);
            matches.then_some(path)
        })
}

fn active_connection_path(conn: &Connection, uuid: &str) -> Option<String> {
    let reply = conn
        .call_method(
            Some(NM_DEST),
            NM_PATH,
            Some(NM_IFACE),
            "ActiveConnections",
            &(),
        )
        .ok()?;
    let paths: Vec<OwnedObjectPath> = reply.body().deserialize().ok()?;
    paths.into_iter().find_map(|path| {
        let path = path.to_string();
        let matches = prop::<String>(conn, &path, ACTIVE_IFACE, "Uuid").as_deref() == Some(uuid);
        matches.then_some(path)
    })
}

/// Typ `nmcli` dla pliku VPN: po rozszerzeniu, a dla `.conf` po treści.
///
/// Wydzielone z [`import`], żeby dało się testować bez magistrali i bez dysku.
fn import_kind(extension: &str, contents: Option<&str>) -> Option<&'static str> {
    match extension {
        "ovpn" => Some("openvpn"),
        "wg" => Some("wireguard"),
        "conf" => Some(conf_kind(contents.unwrap_or(""))),
        _ => None,
    }
}

/// Typ importu dla pliku `.conf`: WireGuard ma sekcje `[Interface]`/`[Peer]`.
fn conf_kind(contents: &str) -> &'static str {
    let lower = contents.to_ascii_lowercase();
    if lower.contains("[interface]") || lower.contains("[peer]") {
        "wireguard"
    } else {
        "openvpn"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn owned_str(value: &str) -> OwnedValue {
        OwnedValue::try_from(Value::from(value.to_owned())).expect("owned value")
    }

    fn connection(id: &str, uuid: &str, kind: &str) -> ConnectionSettings {
        let mut connection = HashMap::new();
        connection.insert("id".to_owned(), owned_str(id));
        connection.insert("uuid".to_owned(), owned_str(uuid));
        connection.insert("type".to_owned(), owned_str(kind));
        let mut settings = HashMap::new();
        settings.insert("connection".to_owned(), connection);
        settings
    }

    #[test]
    fn import_kind_by_extension() {
        assert_eq!(import_kind("ovpn", None), Some("openvpn"));
        assert_eq!(import_kind("wg", None), Some("wireguard"));
        assert_eq!(import_kind("conf", None), Some("openvpn"));
        assert_eq!(import_kind("zip", None), None);
        assert_eq!(import_kind("", None), None);
    }

    #[test]
    fn import_kind_by_conf_content() {
        let wireguard = "[Interface]\nPrivateKey = ...\n[Peer]\nPublicKey = ...\n";
        assert_eq!(import_kind("conf", Some(wireguard)), Some("wireguard"));
        let openvpn = "client\ndev tun\nremote vpn.example.com 1194\n";
        assert_eq!(import_kind("conf", Some(openvpn)), Some("openvpn"));
    }

    #[test]
    fn profiles_are_sorted_active_first_then_name() {
        let saved = vec![
            ("/3".to_owned(), connection("zeta", "uuid-z", "vpn")),
            ("/1".to_owned(), connection("alpha", "uuid-a", "wireguard")),
            ("/2".to_owned(), connection("beta", "uuid-b", "vpn")),
            (
                "/4".to_owned(),
                connection("ethernet", "uuid-e", "802-3-ethernet"),
            ),
        ];
        let active: HashSet<String> = ["uuid-b".to_owned()].into_iter().collect();
        let profiles = profiles_from(&saved, &active);
        let names: Vec<&str> = profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect();
        assert_eq!(names, vec!["beta", "alpha", "zeta"]);
        assert!(profiles[0].active);
    }

    #[test]
    fn active_uuid_matches_profiles() {
        let active: HashSet<String> = ["uuid-a".to_owned()].into_iter().collect();
        let saved = vec![
            ("/1".to_owned(), connection("alpha", "uuid-a", "vpn")),
            ("/2".to_owned(), connection("beta", "uuid-b", "vpn")),
        ];
        let profiles = profiles_from(&saved, &active);
        let alpha = profiles
            .iter()
            .find(|profile| profile.name == "alpha")
            .unwrap();
        let beta = profiles
            .iter()
            .find(|profile| profile.name == "beta")
            .unwrap();
        assert!(alpha.active);
        assert!(!beta.active);
    }
}

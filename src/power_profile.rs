//! Profile zasilania (`power-profiles-daemon`) — D-Bus, magistrala systemowa.
//!
//! Warstwa jest cienka: [`snapshot`] czyta aktywny profil i listę dostępnych,
//! a [`set_profile`] podmienia `ActiveProfile` przez `Properties.Set`. Gdy usługa
//! `net.hadess.PowerProfiles` nie odpowiada, zwracamy stan `available: false`
//! zamiast błędnej migawki.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedValue, Value};

const PP_DEST: &str = "net.hadess.PowerProfiles";
const PP_PATH: &str = "/net/hadess/PowerProfiles";
const PP_IFACE: &str = "net.hadess.PowerProfiles";
const PROPS: &str = "org.freedesktop.DBus.Properties";

/// Standardowa lista profili używana, gdy `Profiles` nie da się odczytać.
const DEFAULT_PROFILES: [&str; 3] = ["power-saver", "balanced", "performance"];

/// Migawka stanu profili zasilania dla UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profiles {
    /// Czy `power-profiles-daemon` odpowiedział.
    pub available: bool,
    /// Aktywny profil (`balanced` domyślnie).
    pub active: String,
    /// Dostępne profile, np. `["power-saver", "balanced", "performance"]`.
    pub choices: Vec<String>,
    pub error: Option<String>,
}

impl Default for Profiles {
    fn default() -> Self {
        Self {
            available: false,
            active: "balanced".to_owned(),
            choices: default_choices(),
            error: None,
        }
    }
}

/// Aktualny profil i lista dostępnych profili.
pub fn snapshot() -> Profiles {
    let mut profiles = Profiles::default();
    let conn = match system() {
        Ok(conn) => conn,
        Err(err) => {
            profiles.error = Some(err);
            return profiles;
        }
    };
    profiles.available = true;
    profiles.active = prop::<String>(&conn, PP_PATH, PP_IFACE, "ActiveProfile")
        .unwrap_or_else(|| "balanced".to_owned());
    profiles.choices = profile_choices(&conn).unwrap_or_else(|_| default_choices());
    profiles
}

/// Przełącza aktywny profil zasilania.
///
/// Najpierw sprawdza (przez [`snapshot`]), czy nazwa istnieje; potem zapisuje
/// `ActiveProfile` przez `Properties.Set`. Błąd, gdy usługa nie odpowiada.
pub fn set_profile(name: &str) -> Result<(), String> {
    let snap = snapshot();
    if !snap.available {
        return Err(snap
            .error
            .unwrap_or_else(|| "PowerProfiles is not available".to_owned()));
    }
    if !snap.choices.iter().any(|choice| choice == name) {
        return Err(format!("Unknown power profile: {name}"));
    }
    let conn = system()?;
    conn.call_method(
        Some(PP_DEST),
        PP_PATH,
        Some(PROPS),
        "Set",
        &(PP_IFACE, "ActiveProfile", Value::from(name.to_owned())),
    )
    .map(|_| ())
    .map_err(|err| format!("PowerProfiles: {err}"))
}

fn system() -> Result<Connection, String> {
    Connection::system().map_err(|err| format!("D-Bus (system): {err}"))
}

fn prop<T>(conn: &Connection, path: &str, iface: &str, name: &str) -> Option<T>
where
    T: TryFrom<OwnedValue>,
{
    let reply = conn
        .call_method(Some(PP_DEST), path, Some(PROPS), "Get", &(iface, name))
        .ok()?;
    let value: OwnedValue = reply.body().deserialize().ok()?;
    T::try_from(value).ok()
}

/// Czyta `Profiles` (`aa{sv}`), wyciągając klucz `Profile` z każdego wpisu.
fn profile_choices(conn: &Connection) -> Result<Vec<String>, String> {
    let reply = conn
        .call_method(
            Some(PP_DEST),
            PP_PATH,
            Some(PROPS),
            "Get",
            &(PP_IFACE, "Profiles"),
        )
        .map_err(|err| format!("PowerProfiles: {err}"))?;
    let value: OwnedValue = reply
        .body()
        .deserialize()
        .map_err(|err| format!("PowerProfiles: {err}"))?;
    let entries: Vec<HashMap<String, OwnedValue>> =
        Vec::try_from(value).map_err(|err| format!("PowerProfiles: {err}"))?;
    let mut choices: Vec<String> = entries
        .into_iter()
        .filter_map(|entry| {
            entry
                .get("Profile")
                .and_then(|value| String::try_from(value.clone()).ok())
        })
        .collect();
    choices.sort();
    choices.dedup();
    if choices.is_empty() {
        return Err("PowerProfiles: empty profile list".to_owned());
    }
    Ok(choices)
}

fn default_choices() -> Vec<String> {
    DEFAULT_PROFILES
        .iter()
        .map(|profile| (*profile).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profiles_are_balanced_and_offline() {
        let profiles = Profiles::default();
        assert!(!profiles.available);
        assert_eq!(profiles.active, "balanced");
        assert_eq!(
            profiles.choices,
            vec!["power-saver", "balanced", "performance"]
        );
        assert_eq!(profiles.error, None);
    }
}

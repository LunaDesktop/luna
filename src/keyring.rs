//! `luna-keyring` — usługa przechowywania haseł (Secret Service API).
//!
//! Implementacja standardu FreeDesktop Secret Service (`org.freedesktop.secrets`):
//! - `org.freedesktop.Secret.Service` na `/org/freedesktop/secrets`
//! - `org.freedesktop.Secret.Collection` na `/org/freedesktop/secrets/collection/default`
//! - `org.freedesktop.Secret.Item` na `/org/freedesktop/secrets/collection/default/<id>`
//! - `org.freedesktop.Secret.Session` na `/org/freedesktop/secrets/session/plain`
//!
//! Dane są zapisywane w pliku `~/.local/share/luna/keyrings/default.keyring`
//! z uprawnieniami `0600` (dostęp tylko dla użytkownika).

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use zbus::zvariant::{self, OwnedObjectPath, OwnedValue, Type};

/// Struktura `Secret` zgodna ze specyfikacją Secret Service API.
#[derive(Type, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Secret {
    pub session: OwnedObjectPath,
    pub parameters: Vec<u8>,
    pub value: Vec<u8>,
    pub content_type: String,
}

/// Pojedynczy wpis w bazie haseł (sekret wraz z atrybutami).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyringItem {
    pub id: String,
    pub label: String,
    pub attributes: HashMap<String, String>,
    pub secret: Vec<u8>,
    pub content_type: String,
    pub created: u64,
    pub modified: u64,
}

/// Magazyn haseł zapisywany na dysku.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    path: PathBuf,
    items: Vec<KeyringItem>,
    next_id: u64,
}

impl KeyringStore {
    /// Otwiera istniejący plik magazynu lub tworzy nowy w domyślnej lokalizacji.
    pub fn default_store() -> Self {
        let dir = if let Ok(data) = std::env::var("XDG_DATA_HOME") {
            PathBuf::from(data).join("luna/keyrings")
        } else if let Ok(home) = std::env::var("HOME") {
            PathBuf::from(home).join(".local/share/luna/keyrings")
        } else {
            PathBuf::from("/tmp/luna-keyrings")
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("default.keyring");
        Self::open_or_create(&path)
    }

    pub fn open_or_create(path: &Path) -> Self {
        let mut items = Vec::new();
        let mut max_id: u64 = 0;

        if let Ok(file) = File::open(path) {
            let reader = BufReader::new(file);
            let mut cur_id = String::new();
            let mut cur_label = String::new();
            let mut cur_type = "text/plain".to_owned();
            let mut cur_secret = Vec::new();
            let mut cur_attrs = HashMap::new();
            let mut cur_created = 0u64;
            let mut cur_modified = 0u64;
            let mut in_item = false;

            for line in reader.lines().flatten() {
                let line = line.trim();
                if line.starts_with("[item:") && line.ends_with(']') {
                    if in_item && !cur_id.is_empty() {
                        items.push(KeyringItem {
                            id: cur_id.clone(),
                            label: cur_label.clone(),
                            attributes: cur_attrs.clone(),
                            secret: cur_secret.clone(),
                            content_type: cur_type.clone(),
                            created: cur_created,
                            modified: cur_modified,
                        });
                    }
                    cur_id = line[6..line.len() - 1].trim().to_owned();
                    if let Ok(num) = cur_id.parse::<u64>() {
                        max_id = max_id.max(num);
                    }
                    cur_label.clear();
                    cur_type = "text/plain".to_owned();
                    cur_secret.clear();
                    cur_attrs.clear();
                    cur_created = 0;
                    cur_modified = 0;
                    in_item = true;
                    continue;
                }
                if !in_item {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let (k, v) = (k.trim(), v.trim());
                    if k == "label" {
                        cur_label = v.to_owned();
                    } else if k == "content_type" {
                        cur_type = v.to_owned();
                    } else if k == "created" {
                        cur_created = v.parse().unwrap_or(0);
                    } else if k == "modified" {
                        cur_modified = v.parse().unwrap_or(0);
                    } else if k == "secret" {
                        cur_secret = hex_decode(v);
                    } else if let Some(attr_key) = k.strip_prefix("attr:") {
                        cur_attrs.insert(attr_key.to_owned(), v.to_owned());
                    }
                }
            }

            if in_item && !cur_id.is_empty() {
                items.push(KeyringItem {
                    id: cur_id,
                    label: cur_label,
                    attributes: cur_attrs,
                    secret: cur_secret,
                    content_type: cur_type,
                    created: cur_created,
                    modified: cur_modified,
                });
            }
        }

        Self {
            path: path.to_path_buf(),
            items,
            next_id: max_id + 1,
        }
    }

    /// Zapisuje stan magazynu na dysk z restrykcyjnymi uprawnieniami `0600`.
    pub fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }

        writeln!(file, "# Luna Keyring — Secret Service Storage")?;
        for item in &self.items {
            writeln!(file, "\n[item:{}]", item.id)?;
            writeln!(file, "label={}", item.label)?;
            writeln!(file, "content_type={}", item.content_type)?;
            writeln!(file, "created={}", item.created)?;
            writeln!(file, "modified={}", item.modified)?;
            writeln!(file, "secret={}", hex_encode(&item.secret))?;
            for (ak, av) in &item.attributes {
                writeln!(file, "attr:{ak}={av}")?;
            }
        }
        file.flush()?;
        Ok(())
    }

    /// Dodaje nowy sekret lub zastępuje istniejący, gdy atrybuty są identyczne.
    pub fn create_or_replace_item(
        &mut self,
        label: String,
        attributes: HashMap<String, String>,
        secret: Vec<u8>,
        content_type: String,
        replace: bool,
    ) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if replace {
            if let Some(existing) = self.items.iter_mut().find(|i| i.attributes == attributes) {
                existing.label = label;
                existing.secret = secret;
                existing.content_type = content_type;
                existing.modified = now;
                return existing.id.clone();
            }
        }

        let id = format!("{}", self.next_id);
        self.next_id += 1;
        self.items.push(KeyringItem {
            id: id.clone(),
            label,
            attributes,
            secret,
            content_type,
            created: now,
            modified: now,
        });
        id
    }

    /// Usuwa element o podanym identyfikatorze.
    pub fn delete_item(&mut self, id: &str) -> bool {
        let len_before = self.items.len();
        self.items.retain(|i| i.id != id);
        self.items.len() < len_before
    }

    /// Wyszukuje identyfikatory elementów, których atrybuty zawierają wszystkie szukane pary.
    pub fn search_items(&self, query: &HashMap<String, String>) -> Vec<String> {
        self.items
            .iter()
            .filter(|item| query.iter().all(|(k, v)| item.attributes.get(k) == Some(v)))
            .map(|item| item.id.clone())
            .collect()
    }

    pub fn get_item(&self, id: &str) -> Option<&KeyringItem> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn get_item_mut(&mut self, id: &str) -> Option<&mut KeyringItem> {
        self.items.iter_mut().find(|i| i.id == id)
    }

    pub fn items(&self) -> &[KeyringItem] {
        &self.items
    }

    /// Następny identyfikator, jaki dostanie nowy wpis.
    pub fn next_id(&self) -> u64 {
        self.next_id
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks(2) {
        if chunk.len() == 2 {
            if let Ok(val) = u8::from_str_radix(std::str::from_utf8(chunk).unwrap_or("00"), 16) {
                out.push(val);
            }
        }
    }
    out
}

// ── Interfejsy D-Bus Secret Service ──

/// Główna usługa Secret Service (`/org/freedesktop/secrets`).
pub struct SecretService {
    pub store: Arc<Mutex<KeyringStore>>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl SecretService {
    fn open_session(
        &self,
        algorithm: &str,
        _input: zvariant::Value<'_>,
    ) -> zbus::fdo::Result<(zvariant::Value<'_>, OwnedObjectPath)> {
        // Obsługujemy tylko sesję `plain`. libsecret przy starcie próbuje `dh`
        // i — gdy usługa odpowie `NotSupported` — wraca do `plain`. Wcześniej
        // zwracaliśmy sukces dla `dh`, przez co klient szyfrował sekret kluczem,
        // którego nigdy nie wymieniliśmy: zapis „działał”, ale odczyt nie.
        if algorithm != "plain" {
            return Err(zbus::fdo::Error::NotSupported(format!(
                "unsupported session algorithm: {algorithm}"
            )));
        }
        Ok((
            zvariant::Value::from(""),
            OwnedObjectPath::try_from("/org/freedesktop/secrets/session/plain").unwrap(),
        ))
    }

    fn create_collection(
        &self,
        _properties: HashMap<String, OwnedValue>,
        _alias: &str,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        Ok((
            OwnedObjectPath::try_from("/org/freedesktop/secrets/collection/default").unwrap(),
            OwnedObjectPath::try_from("/").unwrap(),
        ))
    }

    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> zbus::fdo::Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>)> {
        let store = self.store.lock().unwrap();
        let ids = store.search_items(&attributes);
        let unlocked: Vec<OwnedObjectPath> = ids
            .iter()
            .filter_map(|id| {
                OwnedObjectPath::try_from(format!(
                    "/org/freedesktop/secrets/collection/default/{id}"
                ))
                .ok()
            })
            .collect();
        Ok((unlocked, Vec::new()))
    }

    fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
    ) -> zbus::fdo::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        Ok((objects, OwnedObjectPath::try_from("/").unwrap()))
    }

    fn lock(
        &self,
        _objects: Vec<OwnedObjectPath>,
    ) -> zbus::fdo::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        Ok((Vec::new(), OwnedObjectPath::try_from("/").unwrap()))
    }

    fn get_secrets(
        &self,
        items: Vec<OwnedObjectPath>,
        _session: OwnedObjectPath,
    ) -> zbus::fdo::Result<HashMap<OwnedObjectPath, Secret>> {
        let store = self.store.lock().unwrap();
        let mut map = HashMap::new();
        for path in items {
            let path_str = path.as_str();
            if let Some(id) = path_str.strip_prefix("/org/freedesktop/secrets/collection/default/")
            {
                if let Some(item) = store.get_item(id) {
                    map.insert(
                        path.clone(),
                        Secret {
                            session: OwnedObjectPath::try_from(
                                "/org/freedesktop/secrets/session/plain",
                            )
                            .unwrap(),
                            parameters: Vec::new(),
                            value: item.secret.clone(),
                            content_type: item.content_type.clone(),
                        },
                    );
                }
            }
        }
        Ok(map)
    }

    fn read_alias(&self, name: &str) -> zbus::fdo::Result<OwnedObjectPath> {
        if name == "default" || name == "login" {
            Ok(OwnedObjectPath::try_from("/org/freedesktop/secrets/collection/default").unwrap())
        } else {
            Ok(OwnedObjectPath::try_from("/").unwrap())
        }
    }

    fn set_alias(&self, _name: &str, _collection: OwnedObjectPath) -> zbus::fdo::Result<()> {
        Ok(())
    }

    #[zbus(property)]
    fn collections(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from("/org/freedesktop/secrets/collection/default").unwrap()]
    }
}

/// Domyślna kolekcja (`/org/freedesktop/secrets/collection/default`).
pub struct SecretCollection {
    pub store: Arc<Mutex<KeyringStore>>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
impl SecretCollection {
    fn delete(&self) -> zbus::fdo::Result<OwnedObjectPath> {
        Ok(OwnedObjectPath::try_from("/").unwrap())
    }

    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> zbus::fdo::Result<Vec<OwnedObjectPath>> {
        let store = self.store.lock().unwrap();
        let ids = store.search_items(&attributes);
        let items: Vec<OwnedObjectPath> = ids
            .iter()
            .filter_map(|id| {
                OwnedObjectPath::try_from(format!(
                    "/org/freedesktop/secrets/collection/default/{id}"
                ))
                .ok()
            })
            .collect();
        Ok(items)
    }

    fn create_item(
        &self,
        properties: HashMap<String, OwnedValue>,
        secret: Secret,
        replace: bool,
    ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let label = properties
            .get("org.freedesktop.Secret.Item.Label")
            .and_then(|v| String::try_from(v.clone()).ok())
            .unwrap_or_else(|| "Secret".to_owned());

        let attributes: HashMap<String, String> = properties
            .get("org.freedesktop.Secret.Item.Attributes")
            .and_then(|v| HashMap::<String, String>::try_from(v.clone()).ok())
            .unwrap_or_default();

        let mut store = self.store.lock().unwrap();
        let id = store.create_or_replace_item(
            label,
            attributes,
            secret.value,
            secret.content_type,
            replace,
        );
        let _ = store.save();

        let path =
            OwnedObjectPath::try_from(format!("/org/freedesktop/secrets/collection/default/{id}"))
                .unwrap();
        Ok((path, OwnedObjectPath::try_from("/").unwrap()))
    }

    #[zbus(property)]
    fn items(&self) -> Vec<OwnedObjectPath> {
        let store = self.store.lock().unwrap();
        store
            .items()
            .iter()
            .filter_map(|i| {
                OwnedObjectPath::try_from(format!(
                    "/org/freedesktop/secrets/collection/default/{}",
                    i.id
                ))
                .ok()
            })
            .collect()
    }

    #[zbus(property)]
    fn label(&self) -> String {
        "Luna Keyring".to_owned()
    }

    #[zbus(property)]
    fn locked(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn created(&self) -> u64 {
        0
    }

    #[zbus(property)]
    fn modified(&self) -> u64 {
        0
    }
}

/// Odpowiedź `Item.GetSecret` w formie, jakiej oczekuje GLib/libsecret.
///
/// Kanoniczne XML Secret Service deklaruje jeden argument wyjściowy typu `(oayays)`,
/// ale GDBus (a więc gnome-keyring i libsecret) zawsze opakowuje argumenty metody
/// w strukturę treści, więc na magistrali leci `((oayays))`. Bez tego dodatkowego
/// poziomu `secret_item_load_secret` kończy się błędem:
/// „returned type (oayays), but expected ((oayays))".
#[derive(Type, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SecretReply {
    pub secret: Secret,
}

/// Argument `Item.SetSecret` w formie, jakiej używa libsecret (patrz [`SecretReply`]).
///
/// Klienci zbus (`secret-service`, a więc np. `keyring` w Zedzie) wysyłają pojedynczą
/// strukturę `(oayays)`; zbus toleruje oba warianty, więc nie trzeba wybierać między
/// standardem a libsecret.
#[derive(Type, serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SecretArg {
    pub secret: Secret,
}

/// Sesja (`/org/freedesktop/secrets/session/plain`).
pub struct SecretSession;

#[zbus::interface(name = "org.freedesktop.Secret.Session")]
impl SecretSession {
    fn close(&self) {}
}

/// Pojedynczy wpis (`/org/freedesktop/secrets/collection/default/<id>`).
pub struct SecretItem {
    pub id: String,
    pub store: Arc<Mutex<KeyringStore>>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl SecretItem {
    fn delete(&self) -> zbus::fdo::Result<OwnedObjectPath> {
        let mut store = self.store.lock().unwrap();
        store.delete_item(&self.id);
        let _ = store.save();
        Ok(OwnedObjectPath::try_from("/").unwrap())
    }

    fn get_secret(&self, _session: OwnedObjectPath) -> zbus::fdo::Result<SecretReply> {
        let store = self.store.lock().unwrap();
        let item = store
            .get_item(&self.id)
            .ok_or_else(|| zbus::fdo::Error::Failed("No such secret".into()))?;
        Ok(SecretReply {
            secret: Secret {
                session: OwnedObjectPath::try_from("/org/freedesktop/secrets/session/plain")
                    .unwrap(),
                parameters: Vec::new(),
                value: item.secret.clone(),
                content_type: item.content_type.clone(),
            },
        })
    }

    fn set_secret(&self, secret: SecretArg) -> zbus::fdo::Result<()> {
        let secret = secret.secret;
        let mut store = self.store.lock().unwrap();
        if let Some(item) = store.get_item_mut(&self.id) {
            item.secret = secret.value;
            item.content_type = secret.content_type;
            let _ = store.save();
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed("No such secret".into()))
        }
    }

    #[zbus(property)]
    fn locked(&self) -> bool {
        false
    }

    /// Flagi wpisu (0 = brak specjalnych zachowań).
    #[zbus(property)]
    fn flags(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn attributes(&self) -> HashMap<String, String> {
        let store = self.store.lock().unwrap();
        store
            .get_item(&self.id)
            .map(|i| i.attributes.clone())
            .unwrap_or_default()
    }

    /// Zapis atrybutów wpisu (`libsecret` używa go w `secret_item_set_attributes`).
    /// Bez tego settera D-Bus odpowiadał „property is not writable”.
    #[zbus(property)]
    fn set_attributes(&self, attributes: HashMap<String, String>) -> zbus::fdo::Result<()> {
        let mut store = self.store.lock().unwrap();
        if let Some(item) = store.get_item_mut(&self.id) {
            item.attributes = attributes;
            item.modified = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(item.modified);
            let _ = store.save();
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed("No such secret".into()))
        }
    }

    #[zbus(property)]
    fn label(&self) -> String {
        let store = self.store.lock().unwrap();
        store
            .get_item(&self.id)
            .map(|i| i.label.clone())
            .unwrap_or_default()
    }

    #[zbus(property)]
    fn set_label(&self, label: String) -> zbus::fdo::Result<()> {
        let mut store = self.store.lock().unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(item) = store.get_item_mut(&self.id) {
            item.label = label;
            item.modified = now;
            let _ = store.save();
        }
        Ok(())
    }

    #[zbus(property)]
    fn created(&self) -> u64 {
        let store = self.store.lock().unwrap();
        store.get_item(&self.id).map(|i| i.created).unwrap_or(0)
    }

    #[zbus(property)]
    fn modified(&self) -> u64 {
        let store = self.store.lock().unwrap();
        store.get_item(&self.id).map(|i| i.modified).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_get_reply_is_double_wrapped_for_glib() {
        // libsecret/GDBus oczekują treści metody jako krotki argumentów, więc pojedyncza
        // struktura `Secret` musi być opakowana jeszcze raz. Zmiana tej sygnatury z powrotem
        // na `(oayays)` psuje `secret_item_load_secret` w każdej aplikacji GTK.
        assert_eq!(SecretReply::SIGNATURE.to_string(), "((oayays))");
        assert_eq!(SecretArg::SIGNATURE.to_string(), "((oayays))");
    }

    #[test]
    fn hex_encode_decode_roundtrip() {
        let data = b"Hello, \x00\xff Secret Luna Keyring!";
        let encoded = hex_encode(data);
        let decoded = hex_decode(&encoded);
        assert_eq!(data.as_slice(), decoded.as_slice());
    }

    #[test]
    fn store_create_search_and_persistence() {
        let dir = std::env::temp_dir().join(format!("luna-keyring-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("test.keyring");
        let _ = std::fs::remove_file(&file_path);

        let mut store = KeyringStore::open_or_create(&file_path);
        let mut attrs = HashMap::new();
        attrs.insert("service".to_owned(), "wifi".to_owned());
        attrs.insert("ssid".to_owned(), "Dom-5G".to_owned());

        let id = store.create_or_replace_item(
            "Dom Wi-Fi".to_owned(),
            attrs.clone(),
            b"secret_password_123".to_vec(),
            "text/plain".to_owned(),
            true,
        );
        store.save().unwrap();

        // Ponowny odczyt z pliku
        let store2 = KeyringStore::open_or_create(&file_path);
        let found = store2.search_items(&attrs);
        assert_eq!(found, vec![id.clone()]);

        let item = store2.get_item(&id).unwrap();
        assert_eq!(item.label, "Dom Wi-Fi");
        assert_eq!(item.secret, b"secret_password_123");

        let _ = std::fs::remove_file(&file_path);
    }
}

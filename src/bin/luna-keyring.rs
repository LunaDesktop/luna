//! `luna-keyring` — usługa przechowywania haseł i kluczy (Secret Service daemon).
//!
//! Uruchamiana w sesji użytkownika. Odpowiada na zapytania aplikacji takich jak
//! przeglądarki, klienty poczty, git credential helper, NetworkManager itp.

use std::error::Error;
use std::sync::{Arc, Mutex};

use luna::keyring::{KeyringStore, SecretCollection, SecretItem, SecretService, SecretSession};

fn main() -> Result<(), Box<dyn Error>> {
    let _ = libluna_core::runtime::ensure_runtime_dir();

    let store = Arc::new(Mutex::new(KeyringStore::default_store()));
    println!("luna-keyring: start usługi Secret Service");

    let server_service = SecretService {
        store: Arc::clone(&store),
    };
    let server_col = SecretCollection {
        store: Arc::clone(&store),
    };
    let server_alias = SecretCollection {
        store: Arc::clone(&store),
    };

    let mut builder = zbus::blocking::connection::Builder::session()?
        .name("org.freedesktop.secrets")?
        .serve_at("/org/freedesktop/secrets", server_service)?
        .serve_at("/org/freedesktop/secrets/collection/default", server_col)?
        .serve_at("/org/freedesktop/secrets/aliases/default", server_alias)?
        .serve_at("/org/freedesktop/secrets/session/plain", SecretSession)?;

    // zbus nie pozwala rejestrować obiektów z wnętrza metody D-Bus (blokada serwera
    // obiektów), a `CreateItem` zwraca ścieżkę, po której klient od razu czyta
    // właściwości. Dlatego rezerwujemy z góry zakres identyfikatorów: istniejące wpisy
    // i zapas na nowe sekrety tworzone w trakcie sesji.
    let slots = {
        let s = store.lock().unwrap();
        s.next_id().saturating_add(1024).min(65536)
    };
    for id in 1..=slots {
        builder = builder.serve_at(
            format!("/org/freedesktop/secrets/collection/default/{id}"),
            SecretItem {
                id: id.to_string(),
                store: Arc::clone(&store),
            },
        )?;
    }

    let _conn = builder.build()?;
    println!("luna-keyring: zarejestrowano na magistrali D-Bus (org.freedesktop.secrets)");
    // Marker gotowości — sesja czeka na niego przed startem aplikacji, żeby libsecret
    // nie trafił na moment, w którym nazwa nie jest jeszcze zajęta.
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        let _ = std::fs::write(std::path::PathBuf::from(rt).join("luna-keyring-ready"), "1");
    }

    // Pętla główna daemona
    loop {
        std::thread::park();
    }
}

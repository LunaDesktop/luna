//! `luna-polkit` — demon agenta uwierzytelniania Polkit dla środowiska Luna.

use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use luna::polkit::{register_agent, show_dialog, verify_agent_request, AuthRequest, PolkitAgent};

fn main() -> Result<(), Box<dyn Error>> {
    let _ = libluna_core::runtime::ensure_runtime_dir();

    // Język z Ustawień Luny — okno autoryzacji ma być w języku sesji.
    libluna_i18n::set_locale(libluna_i18n::preferred_locale());
    libluna_i18n::load_catalogs_from_dirs();

    println!("luna-polkit: start agenta uwierzytelniania Polkit");
    let pending: Arc<Mutex<Option<AuthRequest>>> = Arc::new(Mutex::new(None));

    let agent = PolkitAgent {
        pending: Arc::clone(&pending),
    };

    let object_path = "/org/paganlinux/Luna/PolicyKit1/AuthenticationAgent";
    let conn = zbus::blocking::connection::Builder::system()?
        .serve_at(object_path, agent)?
        .build()?;

    register_agent(&conn, object_path)?;
    println!("luna-polkit: zarejestrowano w PolicyKit1.Authority");

    loop {
        let req = {
            let mut p = pending.lock().unwrap();
            p.take()
        };

        if let Some(req) = req {
            let started = std::time::Instant::now();
            // Helper polkit (setuid) sprawdza hasło przez PAM i po sukcesie sam
            // wysyła `AuthenticationAgentResponse` — nie robimy tego drugi raz.
            let user = req.user.clone();
            let cookie = req.cookie.clone();
            let result = show_dialog(&req, move |password| {
                verify_agent_request(&user, &cookie, password)
            });
            match result {
                Some(_) => eprintln!(
                    "luna-polkit: autoryzacja OK ({}), {:.1} s",
                    req.action_id,
                    started.elapsed().as_secs_f64()
                ),
                None => eprintln!("luna-polkit: autoryzacja anulowana ({})", req.action_id),
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

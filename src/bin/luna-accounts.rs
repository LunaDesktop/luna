//! `luna-accounts` — pomocnik roota do zarządzania kontami (wywoływany przez `sudo`).
//!
//! Centrum ustawień uruchamia go z `sudo -S`; hasło administratora zużywa `sudo`,
//! a nowe hasło konta trafia przez stdin. Dzięki temu żadne hasło nie jest widoczne
//! w `ps` ani w historii powłoki.
//!
//! ```text
//! luna-accounts add <login> <imię i nazwisko>   # hasło nowego konta na stdin
//! luna-accounts set-password <login>            # nowe hasło na stdin
//! ```

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match luna::accounts_service::helper_main(&args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("luna-accounts: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

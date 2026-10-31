//! Dodawanie kont i zmiana haseł — część uruchamiana **jako root** przez `sudo`.
//!
//! Centrum ustawień nie woła `useradd`/`chpasswd` bezpośrednio: robi to pomocnik
//! `luna-accounts`, który waliduje dane po stronie roota i przyjmuje **nowe hasło
//! przez stdin** (nigdy przez argv, żeby nie było go widać w `ps`).
//!
//! ```text
//! luna-accounts add <login> <imię i nazwisko>   # hasło nowego konta na stdin
//! luna-accounts set-password <login>            # nowe hasło na stdin
//! ```
//!
//! Hasło administratora zużywa `sudo -S -v` **przed** uruchomieniem pomocnika
//! (Ustawienia robią to osobno), a pomocnik startuje przez `sudo -n` i dostaje
//! już tylko właściwy payload.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// Najdłuższy login (spójne z limitami `useradd`).
pub const MAX_LOGIN: usize = 32;
/// Najdłuższa nazwa w polu GECOS.
pub const MAX_FULL_NAME: usize = 128;
/// Najdłuższe hasło (żeby nie zapchać stdin i shadow).
pub const MAX_PASSWORD: usize = 256;

/// Sprawdza login: mała litera/`_`, dalej małe litery, cyfry, `_` i `-`.
pub fn validate_login(login: &str) -> Result<(), String> {
    if login.is_empty() || login.len() > MAX_LOGIN {
        return Err("Login musi mieć od 1 do 32 znaków".to_owned());
    }
    let mut chars = login.chars();
    let first = chars.next().unwrap_or('_');
    if !(first.is_ascii_lowercase() || first == '_') {
        return Err("Login musi zaczynać się małą literą lub znakiem _".to_owned());
    }
    if !login
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err("Login może zawierać tylko małe litery, cyfry, _ i -".to_owned());
    }
    Ok(())
}

/// Sprawdza pełną nazwę (GECOS): bez znaków kontrolnych, dwukropka i przecinka.
pub fn validate_full_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.len() > MAX_FULL_NAME {
        return Err("Nazwa jest za długa".to_owned());
    }
    if name.contains(['\n', '\r', '\0', ':', ',']) {
        return Err("Nazwa zawiera niedozwolone znaki".to_owned());
    }
    Ok(())
}

/// Sprawdza hasło: niepuste, bez znaków końca linii (chpasswd czyta liniami).
pub fn validate_password(password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("Hasło nie może być puste".to_owned());
    }
    if password.chars().count() > MAX_PASSWORD {
        return Err("Hasło jest za długie".to_owned());
    }
    if password.contains(['\n', '\r', '\0']) {
        return Err("Hasło zawiera niedozwolone znaki".to_owned());
    }
    Ok(())
}

/// Czy proces działa jako root (`id -u` == 0).
pub fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .is_some_and(|uid| uid.trim() == "0")
}

/// Uruchamia program z danymi na stdin; zwraca czytelny błąd z ostatniej linii stderr.
fn run(program: &str, args: &[&str], input: Option<&str>) -> Result<(), String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|err| format!("{program}: {err}"))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input.as_bytes())
            .map_err(|err| format!("{program}: {err}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("{program}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("nieznany błąd");
    Err(format!("{program}: {detail}"))
}

/// Ustawia hasło istniejącego konta (`chpasswd`, hasło przez stdin).
pub fn set_password(login: &str, password: &str) -> Result<(), String> {
    validate_login(login)?;
    validate_password(password)?;
    run("chpasswd", &[], Some(&format!("{login}:{password}\n")))
}

/// Tworzy konto z katalogiem domowym i powłoką, potem ustawia hasło.
///
/// Gdy ustawienie hasła się nie uda, konto jest usuwane (`userdel -r`), żeby nie
/// zostawić konta bez hasła („połowiczny” stan po błędzie).
pub fn add_user(login: &str, full_name: &str, password: &str) -> Result<(), String> {
    validate_login(login)?;
    validate_full_name(full_name)?;
    validate_password(password)?;

    let name = full_name.trim();
    let mut args = vec!["-m", "-s", "/bin/bash"];
    if !name.is_empty() {
        args.push("-c");
        args.push(name);
    }
    args.push("--");
    args.push(login);
    if let Err(err) = run("useradd", &args, None) {
        return Err(err);
    }
    if let Err(err) = set_password(login, password) {
        // Nie zostawiamy konta bez hasła.
        let _ = run("userdel", &["-r", "--", login], None);
        return Err(format!("{err} (konto zostało usunięte)"));
    }
    Ok(())
}

/// Punkt wejścia pomocnika `luna-accounts`.
pub fn helper_main(args: &[String]) -> Result<(), String> {
    if !is_root() {
        return Err("luna-accounts musi działać jako administrator (sudo)".to_owned());
    }
    let mut password = String::new();
    std::io::stdin()
        .read_to_string(&mut password)
        .map_err(|err| format!("stdin: {err}"))?;
    let password = password.trim_end_matches(['\n', '\r']).to_owned();

    let command = args.first().map(String::as_str).unwrap_or("");
    match command {
        "set-password" => {
            let login = args
                .get(1)
                .ok_or_else(|| "użycie: luna-accounts set-password <login>".to_owned())?;
            set_password(login, &password)
        }
        "add" => {
            let login = args
                .get(1)
                .ok_or_else(|| "użycie: luna-accounts add <login> <nazwa>".to_owned())?;
            // Pełna nazwa może przyjść jako jeden argument (tak robi Ustawienia),
            // ale ręczne wywołanie ze spacjami też ma działać.
            let full_name = args.get(2..).map(|rest| rest.join(" ")).unwrap_or_default();
            add_user(login, &full_name, &password)
        }
        _ => Err("użycie: luna-accounts add <login> <nazwa> | set-password <login>".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_accepts_normal_names() {
        for login in ["ala", "jan_kowalski", "user-2", "_service"] {
            assert!(validate_login(login).is_ok(), "{login}");
        }
    }

    #[test]
    fn login_rejects_bad_names() {
        for login in [
            "",
            "Ala",
            "1ala",
            "ala kowalska",
            "ala:x",
            "-ala",
            "a".repeat(40).as_str(),
        ] {
            assert!(validate_login(login).is_err(), "{login}");
        }
    }

    #[test]
    fn full_name_rejects_injection_characters() {
        assert!(validate_full_name("Ala Kowalska").is_ok());
        assert!(validate_full_name("").is_ok());
        assert!(validate_full_name("Ala\nroot").is_err());
        assert!(validate_full_name("Ala:Kowalska").is_err());
        assert!(validate_full_name("Ala,Biuro").is_err());
    }

    #[test]
    fn password_must_fit_one_line() {
        assert!(validate_password("tajne123").is_ok());
        assert!(validate_password("").is_err());
        assert!(validate_password("a\nb").is_err());
        assert!(validate_password(&"x".repeat(MAX_PASSWORD + 1)).is_err());
    }

    #[test]
    fn helper_requires_root_before_touching_anything() {
        // Testy nie działają jako root — pomocnik musi odmówić, a nie wołać useradd.
        if !is_root() {
            let err = helper_main(&["set-password".to_owned(), "root".to_owned()]).unwrap_err();
            assert!(err.contains("administrator"));
        }
    }
}

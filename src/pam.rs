//! Weryfikacja hasła przez PAM (`libpam`).
//!
//! **Dlaczego nie `polkit-agent-helper-1`:** ten program nie jest sprawdzaczem
//! hasła. Czyta ze stdin *cookie* bieżącej autoryzacji (albo bierze je z `argv[2]`),
//! a po udanym PAM wysyła odpowiedź do `polkitd`. Bez aktywnego cookie kończy się
//! `FAILURE` niezależnie od poprawności hasła. Wcześniejsza implementacja podawała
//! mu hasło jako pierwszą linię — helper brał je za cookie, PAM dostawał puste
//! hasło i **każde** logowanie z ekranu blokady/panelu kończyło się „złym hasłem”.
//! Przy `pam_faillock` (`deny = 4`) dodatkowo blokowało to konto na 10 minut we
//! wszystkich usługach PAM.
//!
//! `libpam` ładujemy przez `dlopen`, żeby brak PAM nie psuł budowania ani startu
//! (brak biblioteki = [`AuthOutcome::Unavailable`] i fallback wołającego).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::{Mutex, OnceLock};

/// Wynik sprawdzenia hasła.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Hasło poprawne (konto nie jest zablokowane).
    Ok,
    /// Hasło nieprawidłowe — wołający może poprosić jeszcze raz.
    WrongPassword,
    /// Konto tymczasowo zablokowane przez `pam_faillock`.
    Locked(Option<String>),
    /// Brak `libpam` albo usługi PAM — można spróbować innej drogi (np. sudo).
    Unavailable,
    /// Inny błąd PAM (z komunikatem do pokazania użytkownikowi).
    Error(String),
}

impl AuthOutcome {
    /// Czy hasło zostało zaakceptowane.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok)
    }
}

const PAM_SUCCESS: c_int = 0;
const PAM_OPEN_ERR: c_int = 1;
const PAM_SYMBOL_ERR: c_int = 2;
const PAM_SERVICE_ERR: c_int = 3;
const PAM_SYSTEM_ERR: c_int = 4;
const PAM_BUF_ERR: c_int = 5;
const PAM_PERM_DENIED: c_int = 6;
const PAM_AUTH_ERR: c_int = 7;
const PAM_CRED_INSUFFICIENT: c_int = 8;
const PAM_AUTHINFO_UNAVAIL: c_int = 9;
const PAM_USER_UNKNOWN: c_int = 10;
const PAM_MAXTRIES: c_int = 11;
const PAM_ACCT_EXPIRED: c_int = 13;
const PAM_CONV_ERR: c_int = 19;

const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: Option<
        unsafe extern "C" fn(
            c_int,
            *mut *mut PamMessage,
            *mut *mut PamResponse,
            *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

type PamStart =
    unsafe extern "C" fn(*const c_char, *const c_char, *const PamConv, *mut *mut c_void) -> c_int;
type PamAuthenticate = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type PamEnd = unsafe extern "C" fn(*mut c_void, c_int) -> c_int;
type PamStrError = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char;

struct PamLib {
    pam_start: PamStart,
    pam_authenticate: PamAuthenticate,
    pam_end: PamEnd,
    pam_strerror: PamStrError,
}

// SAFETY: wskaźniki pochodzą z `dlopen` i są ważne przez cały czas życia procesu.
unsafe impl Send for PamLib {}
unsafe impl Sync for PamLib {}

/// Dane przekazywane do konwersacji PAM (hasło + zebrane komunikaty).
struct ConvData {
    user: CString,
    password: CString,
    messages: Mutex<Vec<String>>,
}

/// Konwersacja PAM: podaje hasło na `PAM_PROMPT_ECHO_OFF`, a komunikaty modułów
/// (`PAM_ERROR_MSG`, np. ostrzeżenie `pam_faillock`) zapisuje do `messages`.
unsafe extern "C" fn conversation(
    num_msg: c_int,
    messages: *mut *mut PamMessage,
    responses: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if responses.is_null() || appdata.is_null() || num_msg <= 0 {
        return PAM_CONV_ERR;
    }
    let data = &*(appdata as *const ConvData);
    let count = num_msg as usize;
    // PAM zwalnia tablicę przez `free`, więc musi pochodzić z `calloc`.
    let array = libc::calloc(count, std::mem::size_of::<PamResponse>()) as *mut PamResponse;
    if array.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..count {
        if messages.is_null() {
            break;
        }
        let message = *messages.add(i);
        if message.is_null() {
            continue;
        }
        match (*message).msg_style {
            PAM_PROMPT_ECHO_OFF => {
                (*array.add(i)).resp = libc::strdup(data.password.as_ptr());
            }
            // Pytanie o nazwę użytkownika (np. w `login`) — już ją znamy.
            PAM_PROMPT_ECHO_ON => {
                (*array.add(i)).resp = libc::strdup(data.user.as_ptr());
            }
            PAM_ERROR_MSG | PAM_TEXT_INFO => {
                if !(*message).msg.is_null() {
                    let text = CStr::from_ptr((*message).msg).to_string_lossy();
                    if let Ok(mut list) = data.messages.lock() {
                        list.push(text.into_owned());
                    }
                }
            }
            _ => {}
        }
    }
    *responses = array;
    PAM_SUCCESS
}

fn lib() -> Option<&'static PamLib> {
    static LIB: OnceLock<Option<PamLib>> = OnceLock::new();
    LIB.get_or_init(|| unsafe { load() }).as_ref()
}

unsafe fn load() -> Option<PamLib> {
    let handle = ["libpam.so.0", "libpam.so"].iter().find_map(|name| {
        let name = CString::new(*name).ok()?;
        let handle = libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            None
        } else {
            Some(handle)
        }
    })?;
    let symbol = |name: &str| -> *mut c_void {
        let mut name = name.to_owned();
        name.push('\0');
        libc::dlsym(handle, name.as_ptr() as *const c_char)
    };
    let start = symbol("pam_start");
    let authenticate = symbol("pam_authenticate");
    let end = symbol("pam_end");
    let strerror = symbol("pam_strerror");
    if start.is_null() || authenticate.is_null() || end.is_null() || strerror.is_null() {
        return None;
    }
    // SAFETY: symbole `libpam` mają dokładnie te sygnatury z `<security/pam_appl.h>`.
    Some(PamLib {
        pam_start: std::mem::transmute::<*mut c_void, PamStart>(start),
        pam_authenticate: std::mem::transmute::<*mut c_void, PamAuthenticate>(authenticate),
        pam_end: std::mem::transmute::<*mut c_void, PamEnd>(end),
        pam_strerror: std::mem::transmute::<*mut c_void, PamStrError>(strerror),
    })
}

/// Nazwa usługi PAM: `LUNA_PAM_SERVICE`, inaczej pierwsza istniejąca spośród
/// `luna`, `polkit-1`, `login`.
pub fn service() -> String {
    if let Ok(value) = std::env::var("LUNA_PAM_SERVICE") {
        if !value.is_empty() {
            return value;
        }
    }
    for name in ["luna", "polkit-1", "login"] {
        if std::path::Path::new(&format!("/etc/pam.d/{name}")).exists() {
            return name.to_owned();
        }
    }
    "login".to_owned()
}

/// Sprawdza hasło użytkownika w domyślnej usłudze PAM.
pub fn verify(user: &str, password: &str) -> AuthOutcome {
    verify_with_service(user, password, &service())
}

/// Sprawdza hasło w konkretnej usłudze PAM.
pub fn verify_with_service(user: &str, password: &str, service: &str) -> AuthOutcome {
    let Some(lib) = lib() else {
        return AuthOutcome::Unavailable;
    };
    if user.is_empty() {
        return AuthOutcome::WrongPassword;
    }
    let (Ok(service), Ok(user), Ok(password)) = (
        CString::new(service),
        CString::new(user),
        CString::new(password),
    ) else {
        return AuthOutcome::WrongPassword;
    };
    let data = ConvData {
        user,
        password,
        messages: Mutex::new(Vec::new()),
    };
    let conv = PamConv {
        conv: Some(conversation),
        appdata_ptr: &data as *const ConvData as *mut c_void,
    };
    let mut handle: *mut c_void = ptr::null_mut();
    let status =
        unsafe { (lib.pam_start)(service.as_ptr(), data.user.as_ptr(), &conv, &mut handle) };
    if status != PAM_SUCCESS {
        let outcome = map_status(lib, handle, status, &data.messages);
        if !handle.is_null() {
            unsafe { (lib.pam_end)(handle, status) };
        }
        return outcome;
    }
    let status = unsafe { (lib.pam_authenticate)(handle, 0) };
    let outcome = map_status(lib, handle, status, &data.messages);
    unsafe { (lib.pam_end)(handle, status) };
    outcome
}

fn map_status(
    lib: &PamLib,
    handle: *mut c_void,
    status: c_int,
    messages: &Mutex<Vec<String>>,
) -> AuthOutcome {
    if status == PAM_SUCCESS {
        return AuthOutcome::Ok;
    }
    let message = || {
        let base = unsafe {
            let ptr = (lib.pam_strerror)(handle, status);
            if ptr.is_null() {
                String::new()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().trim().to_owned()
            }
        };
        let detail = messages.lock().ok().and_then(|list| {
            list.iter()
                .rev()
                .find(|line| !line.trim().is_empty())
                .cloned()
        });
        match (base.is_empty(), detail) {
            (false, Some(detail)) => Some(format!("{base} — {detail}")),
            (false, None) => Some(base),
            (true, detail) => detail,
        }
    };
    match status {
        PAM_AUTH_ERR | PAM_CRED_INSUFFICIENT | PAM_USER_UNKNOWN | PAM_PERM_DENIED => {
            AuthOutcome::WrongPassword
        }
        PAM_MAXTRIES => AuthOutcome::Locked(message()),
        PAM_ACCT_EXPIRED => {
            AuthOutcome::Error(message().unwrap_or_else(|| "konto wygasło".to_owned()))
        }
        // Nie da się odczytać danych uwierzytelniania (np. brak `unix_chkpwd`) —
        // wołający może spróbować innej metody.
        PAM_AUTHINFO_UNAVAIL => AuthOutcome::Unavailable,
        PAM_SERVICE_ERR | PAM_SYSTEM_ERR | PAM_BUF_ERR | PAM_CONV_ERR | PAM_OPEN_ERR
        | PAM_SYMBOL_ERR => {
            AuthOutcome::Error(message().unwrap_or_else(|| format!("błąd PAM ({status})")))
        }
        _ => AuthOutcome::Error(message().unwrap_or_else(|| format!("błąd PAM ({status})"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_prefers_existing_config() {
        // Na maszynie deweloperskiej istnieje `polkit-1`; kontrakt: nazwa niepusta.
        assert!(!service().is_empty());
    }

    #[test]
    fn wrong_password_is_reported_without_panic() {
        // Nie znamy hasła testowego — sprawdzamy tylko, że ścieżka PAM jest bezpieczna.
        let outcome = verify("__luna_nie_ma_takiego_usera__", "zle-haslo");
        assert!(!outcome.is_ok());
        assert!(matches!(
            outcome,
            AuthOutcome::WrongPassword | AuthOutcome::Unavailable | AuthOutcome::Error(_)
        ));
    }

    #[test]
    fn empty_user_is_rejected_before_pam() {
        assert_eq!(verify("", "x"), AuthOutcome::WrongPassword);
    }
}

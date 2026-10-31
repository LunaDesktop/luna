//! `luna-apps` — wypisuje aplikacje wykryte z plików `.desktop` (diagnostyka launchera).
//!
//! Uruchom: `cargo run -p luna --bin luna-apps`

fn main() {
    let apps = luna::apps::scan();
    println!("wykryto {} aplikacji:", apps.len());
    for app in &apps {
        let icon = luna::apps::icon_for(app);
        println!(
            "  {:<28} {:<10} {}",
            app.name,
            format!("{icon:?}"),
            app.exec
        );
    }
}

//! Try the label scripts against the Steam client on this PC, on a throwaway label:
//! make it, rename it, put a game in it and take it out, then delete it.
//!
//! ```powershell
//! cargo run --example steam_labels -- 105600   # an app ID you own
//! ```

use windows_link::steam::client::{CefClient, Client, parse_collections, scripts};

fn main() {
    let app_id: u32 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .expect("usage: steam_labels <app ID you own>");
    let client = CefClient::new();
    let run = |what: &str, script: String| {
        let result = client.evaluate(&script);
        println!("{what}: {result:?}");
        result.expect("the step failed")
    };
    let find = |id: &str| {
        let all = parse_collections(&run("list", scripts::collections())).unwrap();
        all.into_iter().find(|c| c.id == id)
    };
    let made = run("create", scripts::create("windows-link test"));
    let id = made["id"].as_str().expect("an id").to_owned();
    println!("  listed: {:?}", find(&id).map(|c| c.name));
    run("rename", scripts::rename(&id, "windows-link test 2"));
    println!("  listed: {:?}", find(&id).map(|c| c.name));
    run("add", scripts::set(&id, app_id, true));
    println!("  apps: {:?}", find(&id).map(|c| c.apps));
    run("remove", scripts::set(&id, app_id, false));
    println!("  apps: {:?}", find(&id).map(|c| c.apps));
    run("delete", scripts::delete(&id));
    println!("  listed: {:?}", find(&id).map(|c| c.name));
    println!(
        "hidden refuses delete: {:?}",
        client.evaluate(&scripts::delete("hidden"))
    );
}

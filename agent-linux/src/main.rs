#[cfg(target_family = "unix")]
mod linux;

#[cfg(all(test, not(target_family = "unix")))]
#[allow(dead_code)]
#[path = "config.rs"]
mod config;
#[cfg(all(test, not(target_family = "unix")))]
#[allow(dead_code)]
#[path = "config_edit.rs"]
mod config_edit;
#[cfg(all(test, not(target_family = "unix")))]
#[allow(dead_code)]
#[path = "smartctl.rs"]
mod smartctl;

#[cfg(target_family = "unix")]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(not(target_family = "unix"))]
fn main() {
    eprintln!("agent-linux targets Linux guests");
}

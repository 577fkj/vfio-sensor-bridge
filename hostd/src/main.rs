#[cfg(target_family = "unix")]
mod linux;

#[cfg(target_family = "unix")]
fn main() -> anyhow::Result<()> {
    linux::run()
}

#[cfg(not(target_family = "unix"))]
fn main() {
    eprintln!("hostd targets Linux/Unix hosts");
}

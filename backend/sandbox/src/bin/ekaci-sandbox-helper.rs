#[cfg(target_os = "linux")]
fn main() {
    let args = std::env::args_os().skip(1).collect();
    if let Err(e) = sandbox::helper::run(args) {
        eprintln!("ekaci-sandbox-helper: {e:#}");
        std::process::exit(126);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("ekaci-sandbox-helper: Linux only");
    std::process::exit(126);
}

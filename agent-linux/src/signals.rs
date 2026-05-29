use std::sync::atomic::{AtomicBool, Ordering};

static RUNNING: AtomicBool = AtomicBool::new(true);

pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

pub fn install_signal_handlers() {
    unsafe {
        let handler = handle_shutdown_signal as *const () as libc::sighandler_t;

        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

extern "C" fn handle_shutdown_signal(_: libc::c_int) {
    RUNNING.store(false, Ordering::Relaxed);
}

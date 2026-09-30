use std::sync::atomic::{AtomicBool, Ordering};

static STOP: AtomicBool = AtomicBool::new(false);

pub fn requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

#[cfg(unix)]
extern "C" fn handler(_signal: libc::c_int) {
    // A lock-free atomic store is the only work performed in the signal handler.
    STOP.store(true, Ordering::Relaxed);
}

#[cfg(unix)]
pub struct Guard {
    interrupt: libc::sighandler_t,
    terminate: libc::sighandler_t,
}

#[cfg(unix)]
impl Guard {
    pub fn install() -> Result<Self, String> {
        STOP.store(false, Ordering::Relaxed);
        // SAFETY: handler has the C signal ABI and never allocates or unwinds.
        unsafe {
            let interrupt = libc::signal(libc::SIGINT, handler as *const () as libc::sighandler_t);
            if interrupt == libc::SIG_ERR {
                return Err("无法注册停止信号。".into());
            }
            let terminate = libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
            if terminate == libc::SIG_ERR {
                libc::signal(libc::SIGINT, interrupt);
                return Err("无法注册停止信号。".into());
            }
            Ok(Self {
                interrupt,
                terminate,
            })
        }
    }
}

#[cfg(unix)]
impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: restore the handlers returned by signal when this guard was installed.
        unsafe {
            libc::signal(libc::SIGINT, self.interrupt);
            libc::signal(libc::SIGTERM, self.terminate);
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
}

#[cfg(windows)]
unsafe extern "system" fn console_handler(event: u32) -> i32 {
    if matches!(event, 0 | 1 | 2 | 5 | 6) {
        STOP.store(true, Ordering::Relaxed);
        1
    } else {
        0
    }
}

#[cfg(windows)]
pub struct Guard;

#[cfg(windows)]
impl Guard {
    pub fn install() -> Result<Self, String> {
        STOP.store(false, Ordering::Relaxed);
        // A hidden task may have no console. Local stop requests remain available.
        // SAFETY: callback has the documented Windows handler ABI and a static lifetime.
        unsafe {
            SetConsoleCtrlHandler(Some(console_handler), 1);
        }
        Ok(Self)
    }
}

#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        // SAFETY: remove the same static callback registered by this guard.
        unsafe {
            SetConsoleCtrlHandler(Some(console_handler), 0);
        }
    }
}

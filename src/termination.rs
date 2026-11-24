use std::{collections::HashMap, io};

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuditToken([u32; 8]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    pid: i32,
    token: AuditToken,
}

impl ProcessIdentity {
    pub fn pid(self) -> i32 {
        self.pid
    }

    #[cfg(test)]
    pub fn fixture(pid: i32, generation: u32) -> Self {
        Self {
            pid,
            token: AuditToken([0, 0, 0, 0, 0, pid as u32, 0, generation]),
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::{capture_identities, process_identity, send_sigkill};

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::{mem, ptr, sync::OnceLock};

    const TASK_AUDIT_TOKEN: libc::task_flavor_t = 15;
    type SignalWithToken = unsafe extern "C" fn(*mut AuditToken, libc::c_int) -> libc::c_int;

    unsafe extern "C" {
        fn task_name_for_pid(
            caller: libc::mach_port_t,
            pid: libc::pid_t,
            name: *mut libc::mach_port_t,
        ) -> libc::kern_return_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }

    #[link(name = "bsm")]
    unsafe extern "C" {
        fn audit_token_to_pid(token: AuditToken) -> libc::pid_t;
    }

    pub fn capture_identities() -> HashMap<i32, ProcessIdentity> {
        // A null buffer asks libproc how much PID storage to allocate.
        let capacity = unsafe { libc::proc_listallpids(ptr::null_mut(), 0) };
        if capacity <= 0 {
            return HashMap::new();
        }
        let mut pids = vec![0i32; capacity as usize];
        let size = mem::size_of_val(pids.as_slice()) as libc::c_int;
        // The buffer has space for size bytes. libproc returns the count written.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
        pids.truncate(count.max(0) as usize);
        pids.into_iter()
            .filter_map(|pid| process_identity(pid).map(|identity| (pid, identity)))
            .collect()
    }

    pub fn process_identity(pid: i32) -> Option<ProcessIdentity> {
        if pid <= 0 {
            return None;
        }
        let mut port = 0;
        // The returned task-name send right identifies a task even if its PID is reused.
        #[allow(deprecated)]
        let caller = unsafe { libc::mach_task_self() };
        let result = unsafe { task_name_for_pid(caller, pid, &mut port) };
        if result != libc::KERN_SUCCESS || port == 0 {
            return None;
        }

        let mut token = AuditToken([0; 8]);
        let mut count = token.0.len() as libc::mach_msg_type_number_t;
        // TASK_AUDIT_TOKEN writes count Mach words. Release the send right regardless of status.
        let result = unsafe {
            let result = libc::task_info(
                port,
                TASK_AUDIT_TOKEN,
                token.0.as_mut_ptr().cast(),
                &mut count,
            );
            mach_port_deallocate(caller, port);
            result
        };
        if result != libc::KERN_SUCCESS || count as usize != token.0.len() {
            return None;
        }
        (unsafe { audit_token_to_pid(token) } == pid).then_some(ProcessIdentity { pid, token })
    }

    pub fn send_sigkill(mut identity: ProcessIdentity) -> io::Result<()> {
        static SENDER: OnceLock<Option<SignalWithToken>> = OnceLock::new();
        let send = SENDER
            .get_or_init(|| {
                // Older macOS versions lack this symbol. Keep the rest of Memo loadable.
                let symbol = unsafe {
                    libc::dlsym(libc::RTLD_DEFAULT, c"proc_signal_with_audittoken".as_ptr())
                };
                if symbol.is_null() {
                    None
                } else {
                    // libproc.h declares this symbol with the SignalWithToken ABI.
                    Some(unsafe { mem::transmute::<*mut libc::c_void, SignalWithToken>(symbol) })
                }
            })
            .ok_or_else(unsupported)?;
        // XNU validates the token and signals the resulting process reference.
        // libproc returns errno directly.
        let error = unsafe { send(&mut identity.token, libc::SIGKILL) };
        if error == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(error))
        }
    }
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "identity-bound SIGKILL is unavailable on this OS",
    )
}

#[cfg(not(target_os = "macos"))]
pub fn capture_identities() -> HashMap<i32, ProcessIdentity> {
    HashMap::new()
}

#[cfg(not(target_os = "macos"))]
pub fn process_identity(_pid: i32) -> Option<ProcessIdentity> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn send_sigkill(_identity: ProcessIdentity) -> io::Result<()> {
    Err(unsupported())
}

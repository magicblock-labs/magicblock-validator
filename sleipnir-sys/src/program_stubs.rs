use std::sync::{Arc, RwLock};

use solana_sdk::program_error::UNSUPPORTED_SYSVAR;

lazy_static::lazy_static! {
    static ref SYSCALL_STUBS: Arc<RwLock<Box<dyn SyscallStubs>>> = Arc::new(RwLock::new(Box::new(DefaultSyscallStubs {})));
}

// The default syscall stubs may not do much, but `set_syscalls()` can be used
// to swap in alternatives
// TODO(thlorenz): To allow accessing the highres clock in Rust tests
// we need to provide a way for users to set it as part of setting up the ProgramTest
// environment, see: solana/program-test/src/lib.rs:234
pub fn set_syscall_stubs(
    syscall_stubs: Box<dyn SyscallStubs>,
) -> Box<dyn SyscallStubs> {
    std::mem::replace(&mut SYSCALL_STUBS.write().unwrap(), syscall_stubs)
}

pub trait SyscallStubs: Sync + Send {
    fn sol_get_highres_clock_sysvar(&self, _var_addr: *mut u8) -> u64 {
        UNSUPPORTED_SYSVAR
    }
}

struct DefaultSyscallStubs {}
impl SyscallStubs for DefaultSyscallStubs {}

pub(crate) fn sol_get_highres_clock_sysvar(var_addr: *mut u8) -> u64 {
    SYSCALL_STUBS
        .read()
        .unwrap()
        .sol_get_highres_clock_sysvar(var_addr)
}

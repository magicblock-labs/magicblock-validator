macro_rules! define_syscall {
    (fn $name:ident($($arg:ident: $typ:ty),*) -> $ret:ty) => {
        #[inline]
        #[allow(clippy::missing_safety_doc)]
        pub unsafe fn $name($($arg: $typ),*) -> $ret {
        // this enum is used to force the hash to be computed in a const context
        #[repr(usize)]
        enum Syscall {
            Code = sys_hash(stringify!($name)),
        }

            let syscall: extern "C" fn($($arg: $typ),*) -> $ret = core::mem::transmute(Syscall::Code);
            syscall($($arg),*)
        }

    };
    (fn $name:ident($($arg:ident: $typ:ty),*)) => {
        define_syscall!(fn $name($($arg: $typ),*) -> ());
    }
}

define_syscall!(fn sol_get_highres_clock_sysvar(addr: *mut u8) -> u64);

// -----------------
// sys_hash
// -----------------
pub const fn sys_hash(name: &str) -> usize {
    murmur3_32(name.as_bytes(), 0) as usize
}
const fn murmur3_32(buf: &[u8], seed: u32) -> u32 {
    const fn pre_mix(buf: [u8; 4]) -> u32 {
        u32::from_le_bytes(buf)
            .wrapping_mul(0xcc9e2d51)
            .rotate_left(15)
            .wrapping_mul(0x1b873593)
    }

    let mut hash = seed;

    let mut i = 0;
    while i < buf.len() / 4 {
        let buf = [buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2], buf[i * 4 + 3]];
        hash ^= pre_mix(buf);
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe6546b64);

        i += 1;
    }

    match buf.len() % 4 {
        0 => {}
        1 => {
            hash ^= pre_mix([buf[i * 4], 0, 0, 0]);
        }
        2 => {
            hash ^= pre_mix([buf[i * 4], buf[i * 4 + 1], 0, 0]);
        }
        3 => {
            hash ^= pre_mix([buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2], 0]);
        }
        _ => { /* unreachable!() */ }
    }

    hash ^= buf.len() as u32;
    hash = hash ^ (hash.wrapping_shr(16));
    hash = hash.wrapping_mul(0x85ebca6b);
    hash = hash ^ (hash.wrapping_shr(13));
    hash = hash.wrapping_mul(0xc2b2ae35);
    hash = hash ^ (hash.wrapping_shr(16));

    hash
}

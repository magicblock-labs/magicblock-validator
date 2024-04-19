use crate::errors::LedgerError;

#[cfg(not(unix))]
pub fn adjust_ulimit_nofile(
    _enforce_ulimit_nofile: bool,
) -> std::result::Result<(), LedgerError> {
    Ok(())
}

#[cfg(unix)]
pub fn adjust_ulimit_nofile(
    enforce_ulimit_nofile: bool,
) -> std::result::Result<(), LedgerError> {
    use log::*;

    // Rocks DB likes to have many open files.  The default open file descriptor limit is
    // usually not enough
    // AppendVecs and disk Account Index are also heavy users of mmapped files.
    // This should be kept in sync with published validator instructions.
    // https://docs.solanalabs.com/operations/guides/validator-start#increased-memory-mapped-files-limit
    let desired_nofile = 1_000_000;

    fn get_nofile() -> libc::rlimit {
        let mut nofile = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut nofile) } != 0 {
            warn!("getrlimit(RLIMIT_NOFILE) failed");
        }
        nofile
    }

    let mut nofile = get_nofile();
    let current = nofile.rlim_cur;
    if current < desired_nofile {
        nofile.rlim_cur = desired_nofile;
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &nofile) } != 0 {
            error!(
                "Unable to increase the maximum open file descriptor limit to {} from {}",
                nofile.rlim_cur, current,
            );

            if cfg!(target_os = "macos") {
                error!(
                    "On mac OS you may need to run |sudo launchctl limit maxfiles {} {}| first",
                    desired_nofile, desired_nofile,
                );
            }
            if enforce_ulimit_nofile {
                return Err(LedgerError::UnableToSetOpenFileDescriptorLimit);
            }
        }

        nofile = get_nofile();
    }
    info!("Maximum open file descriptors: {}", nofile.rlim_cur);
    Ok(())
}

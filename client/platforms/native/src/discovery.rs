//! OS-specific support for background filesystem discovery.
pub fn hidden_directory(_entry: &std::fs::DirEntry) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        return _entry.metadata().map_or(true, |metadata| metadata.file_attributes() & 2 != 0);
    }
    #[cfg(not(windows))]
    false
}

pub fn lower_priority() {
    #[cfg(target_os = "linux")]
    // Linux applies nice and I/O priorities to the calling thread when who=0.
    // Failure is harmless: the portable throttling still limits the walk.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13); // IOPRIO_CLASS_IDLE
    }
}

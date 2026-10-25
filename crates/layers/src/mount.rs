//! Mounting a layer's EROFS file read-only on a snapshot's folder, and taking it away. (Linux's own: on another
//! system, building and testing the rest, a mount says it cannot be.)

use std::path::Path;

/// A path as the kernel takes it (the service's own paths: none has a NUL in it).
#[cfg(target_os = "linux")]
fn cstr(p: &Path) -> std::ffi::CString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).expect("a path with no NUL")
}

/// A system call's answer (0, or what failed).
#[cfg(target_os = "linux")]
fn os(r: libc::c_int) -> std::io::Result<()> { if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) } }

/// An EROFS file mounted read-only at `target` (the kernel reads the file itself: Linux 6.12 and later).
#[cfg(target_os = "linux")]
pub fn mount_erofs(file: &Path, target: &Path) -> std::io::Result<()> {
    let (src, dst) = (cstr(file), cstr(target));
    let fstype = c"erofs";
    crate::faults::check(os(unsafe { libc::mount(src.as_ptr(), dst.as_ptr(), fstype.as_ptr(), libc::MS_RDONLY | libc::MS_NODEV | libc::MS_NOSUID, std::ptr::null()) }))
}

#[cfg(not(target_os = "linux"))]
pub fn mount_erofs(_file: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "EROFS is Linux's"))
}

/// What is mounted at `target` taken away; a mount still in use (a container's overlay reading it) goes once the
/// last user lets go, as `umount -l`.
#[cfg(target_os = "linux")]
pub fn unmount(target: &Path) -> std::io::Result<()> {
    let dst = cstr(target);
    crate::faults::check(os(unsafe { libc::umount2(dst.as_ptr(), libc::MNT_DETACH) }))
}

#[cfg(not(target_os = "linux"))]
pub fn unmount(target: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let dst = std::ffi::CString::new(target.as_os_str().as_bytes()).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e)).and_then(crate::faults::pass)?;
    let r = unsafe { libc::unmount(dst.as_ptr(), libc::MNT_FORCE) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

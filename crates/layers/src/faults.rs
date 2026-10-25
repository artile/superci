//! One of the program's steps that can fail, failing in a test, as SQLite tests itself: every `?` in the program
//! passes through `pass`, and with `SUPERCI_LAYERS_FAULT=N` the N-th to pass (counted from the program's start) fails
//! as the step's own error would (with `SUPERCI_LAYERS_FAULTS=FILE`, how many have passed so far is written to FILE
//! after each, so that a test knows how many there are to fail). In a debug build only: a release build passes all.

/// What passed, passed on; or, in a test, the step asked to fail, failing.
#[cfg(debug_assertions)]
pub fn pass<T, E: From<std::io::Error>>(v: T) -> Result<T, E> {
    static PASSED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = PASSED.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    if let Some(file) = std::env::var_os("SUPERCI_LAYERS_FAULTS") { let _ = std::fs::write(file, n.to_string()); }
    if std::env::var("SUPERCI_LAYERS_FAULT").ok().and_then(|f| f.parse::<usize>().ok()) == Some(n) { return Err(std::io::Error::other(format!("step {n} failed, as the test asked")).into()) }
    Ok(v)
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn pass<T, E>(v: T) -> Result<T, E> { Ok(v) }

/// The same for a step whose error is said in words.
pub fn said<T>(v: T) -> Result<T, String> { pass::<T, std::io::Error>(v).map_err(|e| e.to_string()) }

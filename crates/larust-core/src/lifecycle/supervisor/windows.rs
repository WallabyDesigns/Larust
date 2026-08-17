//! Windows backend for `lifecycle::supervisor`: a single process-wide Job
//! Object every spawned replacement gets assigned to, configured with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` — the OS kills every process still
//! in the job the moment the job object's last handle closes. `xr dev`
//! itself holds that one handle (created lazily below, never duplicated
//! into any child, never explicitly closed) for its entire lifetime, so
//! that "last handle closes" moment is exactly "`xr dev` itself exits, for
//! any reason" — a crash, `taskkill /F`, a closed terminal window — not
//! just the graceful paths (Ctrl+C, the admin channel's `STOP`) this
//! codebase already handles elsewhere.
//!
//! `xr dev`'s own process does *not* need to be explicitly assigned to
//! this job — `KILL_ON_JOB_CLOSE` triggers on the job handle's own
//! lifetime, not on job membership. Windows closes every handle a process
//! holds when that process exits, regardless of how it exits; as long as
//! nothing else ever holds a second handle to this job (the default —
//! handles aren't inherited by spawned children unless explicitly marked),
//! `xr dev` exiting is the only thing that can ever release it.

use std::sync::OnceLock;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// A `HANDLE` is an opaque reference into the kernel's object table (a
/// `*mut c_void` in this `windows-sys` version, but never dereferenced as
/// a pointer to memory this process manages) — safe to share across
/// threads. This wrapper exists only so `OnceLock` has a named, `Send +
/// Sync` type to hold.
struct JobHandle(HANDLE);
// SAFETY: see the doc comment above.
unsafe impl Send for JobHandle {}
// SAFETY: see the doc comment above.
unsafe impl Sync for JobHandle {}

static JOB: OnceLock<Option<JobHandle>> = OnceLock::new();

fn job_handle() -> Option<HANDLE> {
    JOB.get_or_init(create_job).as_ref().map(|h| h.0)
}

fn create_job() -> Option<JobHandle> {
    // SAFETY: an anonymous, unnamed job object (both arguments null) — the
    // documented way to request one with no name-collision risk. A zero
    // return means creation failed; `GetLastError()`'s reason isn't
    // captured since this whole feature is best-effort (see this module's
    // own doc comment).
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        tracing::warn!(
            "failed to create the dev-server supervision job object; spawned replacements \
             won't be auto-killed if this process dies unexpectedly"
        );
        return None;
    }

    // SAFETY: `info` is a plain-old-data struct; zero-initializing it and
    // then only setting the one field this call actually needs
    // (`LimitFlags`) is the documented pattern for
    // `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` — every other field keeps its
    // zero/default meaning ("no limit").
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

    // SAFETY: `handle` was just returned by `CreateJobObjectW` above and
    // is known valid; `info` and its exact size are what
    // `SetInformationJobObject` expects for the
    // `JobObjectExtendedLimitInformation` information class.
    let ok = unsafe {
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        tracing::warn!(
            "failed to configure the dev-server supervision job object; spawned replacements \
             won't be auto-killed if this process dies unexpectedly"
        );
        return None;
    }

    Some(JobHandle(handle))
}

/// Best-effort: assigns `child` to the process-wide supervision job (see
/// this module's own doc comment), creating the job on first use. Logs a
/// warning and does nothing further on any failure — never treated as
/// fatal to the handoff that's spawning `child` in the first place.
pub(super) fn register(child: &tokio::process::Child) {
    let Some(job) = job_handle() else { return };
    // `None` only once the child has already been reaped (its handle
    // closed) -- nothing left to assign to a job at that point, and not
    // the case here (called immediately after a fresh `spawn()`).
    let Some(raw_handle) = child.raw_handle() else {
        return;
    };
    let handle = raw_handle as HANDLE;
    // SAFETY: `handle` is `child`'s own real process handle —
    // `tokio::process::Child` owns it and keeps it valid for at least as
    // long as `child` itself is alive, which it is here; `job` was
    // validated by `create_job` above.
    let ok = unsafe { AssignProcessToJobObject(job, handle) };
    if ok == 0 {
        tracing::warn!(
            "failed to assign a spawned replacement to the dev-server supervision job object; \
             it won't be auto-killed if this process dies unexpectedly"
        );
    }
}

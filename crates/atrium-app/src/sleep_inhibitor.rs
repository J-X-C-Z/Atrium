//! Workspace owns one guard and updates it after evaluating `autopilot.on` and
//! `Project::has_work` across all projects. The request affects local idle
//! system sleep only.

use std::io;

#[derive(Default)]
pub(crate) struct SleepInhibitor {
    request: Option<platform::Request>,
}

impl SleepInhibitor {
    /// Reconcile the request with the current need. A failed acquisition leaves
    /// the guard idle and can be retried. Unsupported platforms return
    /// `io::ErrorKind::Unsupported` when `needed` is true.
    pub(crate) fn set_needed(&mut self, needed: bool) -> io::Result<()> {
        if !needed {
            self.request = None;
        } else if self.request.is_none() {
            self.request = Some(platform::Request::acquire()?);
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CStr, c_char, c_void};
    use std::io;
    use std::ptr::null;

    const UTF8: u32 = 0x0800_0100;
    const ASSERTION_LEVEL_ON: u32 = 255;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            string: *const c_char,
            encoding: u32,
        ) -> *const c_void;
        fn CFRelease(value: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            level: u32,
            name: *const c_void,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    struct CfString(*const c_void);

    impl CfString {
        fn new(string: &CStr) -> io::Result<Self> {
            // SAFETY: The C string is NUL-terminated. NULL selects the default allocator.
            let value = unsafe { CFStringCreateWithCString(null(), string.as_ptr(), UTF8) };
            if value.is_null() {
                return Err(io::Error::other(
                    "could not allocate a power assertion string",
                ));
            }
            Ok(Self(value))
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            // SAFETY: This guard owns one non-null Core Foundation reference.
            unsafe { CFRelease(self.0) };
        }
    }

    pub(super) struct Request(u32);

    impl Request {
        pub(super) fn acquire() -> io::Result<Self> {
            let assertion_type = CfString::new(c"PreventUserIdleSystemSleep")?;
            let name = CfString::new(c"Atrium Autopilot")?;
            let mut id = 0;
            // SAFETY: Both CFStrings are live and `id` is writable for the call.
            let status = unsafe {
                IOPMAssertionCreateWithName(assertion_type.0, ASSERTION_LEVEL_ON, name.0, &mut id)
            };
            if status != 0 {
                return Err(io::Error::other(format!(
                    "IOPMAssertionCreateWithName failed with IOReturn {:#010x}",
                    status as u32
                )));
            }
            Ok(Self(id))
        }
    }

    impl Drop for Request {
        fn drop(&mut self) {
            // SAFETY: A successful creation gave this guard one assertion to release.
            unsafe { IOPMAssertionRelease(self.0) };
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

    const POWER_REQUEST_CONTEXT_VERSION: u32 = 0;
    const POWER_REQUEST_CONTEXT_SIMPLE_STRING: u32 = 1;
    const POWER_REQUEST_SYSTEM_REQUIRED: i32 = 1;

    #[repr(C)]
    struct ReasonContext {
        version: u32,
        flags: u32,
        reason: Reason,
    }

    #[repr(C)]
    union Reason {
        simple: *mut u16,
        // Preserve the SDK union's size and alignment with the simple flag too.
        detailed: DetailedReason,
    }

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct DetailedReason {
        module: RawHandle,
        reason_id: u32,
        string_count: u32,
        strings: *mut *mut u16,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PowerCreateRequest(context: *mut ReasonContext) -> RawHandle;
        fn PowerSetRequest(handle: RawHandle, request_type: i32) -> i32;
        fn PowerClearRequest(handle: RawHandle, request_type: i32) -> i32;
    }

    pub(super) struct Request(OwnedHandle);

    impl Request {
        pub(super) fn acquire() -> io::Result<Self> {
            let mut reason: Vec<u16> = "Atrium Autopilot\0".encode_utf16().collect();
            let mut context = ReasonContext {
                version: POWER_REQUEST_CONTEXT_VERSION,
                flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                reason: Reason {
                    simple: reason.as_mut_ptr(),
                },
            };
            // SAFETY: The context and its NUL-terminated UTF-16 string live through the call.
            let raw = unsafe { PowerCreateRequest(&mut context) };
            if raw == std::ptr::without_provenance_mut(usize::MAX) {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: PowerCreateRequest succeeded and transferred a new handle to us.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            // SAFETY: The owned handle remains live and the request type is valid.
            if unsafe { PowerSetRequest(handle.as_raw_handle(), POWER_REQUEST_SYSTEM_REQUIRED) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(handle))
        }
    }

    impl Drop for Request {
        fn drop(&mut self) {
            // SAFETY: This matches one successful PowerSetRequest on the live handle.
            unsafe { PowerClearRequest(self.0.as_raw_handle(), POWER_REQUEST_SYSTEM_REQUIRED) };
            // OwnedHandle closes the handle even if clearing the request failed.
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::io;

    pub(super) enum Request {}

    impl Request {
        pub(super) fn acquire() -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "idle sleep inhibition is unavailable on this platform",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciles_idle_and_active_requests() {
        let mut inhibitor = SleepInhibitor::default();
        inhibitor.set_needed(false).unwrap();
        assert!(inhibitor.request.is_none());

        #[cfg(any(target_os = "macos", windows))]
        {
            inhibitor.set_needed(true).unwrap();
            inhibitor.set_needed(true).unwrap();
            assert!(inhibitor.request.is_some());
        }

        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let error = inhibitor.set_needed(true).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(inhibitor.request.is_none());
        }

        inhibitor.set_needed(false).unwrap();
        inhibitor.set_needed(false).unwrap();
        assert!(inhibitor.request.is_none());
    }
}

use std::io;

use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// Windows inherits every inheritable handle during ordinary process creation,
/// including original stdio handles when a child's stdio is redirected. A shared
/// service must not retain the editor attachment's pipes and prevent their EOF.
pub(super) fn prevent_inheritance() -> io::Result<()> {
    for stream in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: GetStdHandle returns a borrowed process handle. Updating its
        // inheritance flag neither transfers ownership nor closes the stream.
        unsafe {
            let handle = GetStdHandle(stream);
            if handle.is_null() {
                continue;
            }
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

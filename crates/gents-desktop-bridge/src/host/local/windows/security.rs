use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Win32::Foundation::{LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    CopySid, EqualSid, GetAce, GetLengthSid, GetSecurityDescriptorDacl, GetTokenInformation,
    IsValidAcl, IsWellKnownSid, TokenUser, WinBuiltinAdministratorsSid, WinCreatorOwnerSid,
    WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSID, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

#[derive(Debug, thiserror::Error)]
pub enum WindowsSecurityError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("Windows did not return a user token size")]
    TokenSizeUnavailable,
    #[error("decoding the Windows user SID")]
    SidText(#[source] std::string::FromUtf16Error),
    #[error("client files must be owned by this Windows user, SYSTEM or administrators")]
    UnexpectedOwner,
    #[error("client files require a private Windows DACL")]
    DaclMissing,
    #[error("client files require an explicit private Windows ACL")]
    DaclInvalid,
    #[error("client files use an unsupported Windows ACL entry")]
    UnsupportedAce,
    #[error("client files grant access to another Windows account; choose a private client home under LocalAppData")]
    ForeignGrant,
}

type Result<T, E = WindowsSecurityError> = std::result::Result<T, E>;

pub(super) struct LocalAllocation(pub(super) *mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: each value owns one allocation returned by a Windows LocalAlloc API.
        unsafe { LocalFree(self.0) };
    }
}

pub(super) struct Sid(Vec<u32>);

impl Sid {
    pub(super) fn current() -> Result<Self> {
        // SAFETY: the pseudo-handle remains valid for the lifetime of this process.
        Self::from_process(unsafe { GetCurrentProcess() })
    }

    pub(super) fn for_process(pid: u32) -> Result<Self> {
        // SAFETY: OpenProcess returns an owned handle or null; no rights to modify the peer are requested.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: raw is a newly owned, non-null process handle.
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        Self::from_process(process.as_raw_handle())
    }

    fn from_process(process: HANDLE) -> Result<Self> {
        let mut token = ptr::null_mut();
        // SAFETY: process is a live process handle; token is writable for one returned handle.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: OpenProcessToken transferred ownership of token on success.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut length = 0;
        // SAFETY: a null buffer with zero length queries the required token-information size.
        unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                ptr::null_mut(),
                0,
                &mut length,
            )
        };
        if (length as usize) < size_of::<TOKEN_USER>() {
            return Err(WindowsSecurityError::TokenSizeUnavailable);
        }
        let mut buffer = vec![0_usize; (length as usize).div_ceil(size_of::<usize>())];
        // SAFETY: buffer has TOKEN_USER alignment and at least length initialized bytes.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: successful TokenUser information starts with TOKEN_USER; its SID lives in buffer.
        let raw = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        // SAFETY: GetTokenInformation returned a valid SID that remains live until buffer drops.
        let length = unsafe { GetLengthSid(raw) };
        let mut sid = Self(vec![0; (length as usize).div_ceil(size_of::<u32>())]);
        // SAFETY: sid has SID alignment and length bytes; raw remains valid for this copy.
        if unsafe { CopySid(length, sid.0.as_mut_ptr().cast(), raw) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(sid)
    }

    pub(super) fn matches(&self, other: &Self) -> bool {
        self.matches_raw(other.raw())
    }

    fn matches_raw(&self, other: PSID) -> bool {
        // SAFETY: self owns a copied SID; callers provide another live Windows SID.
        unsafe { EqualSid(self.raw(), other) != 0 }
    }

    fn raw(&self) -> PSID {
        self.0.as_ptr().cast_mut().cast()
    }

    pub(super) fn text(&self) -> Result<String> {
        let mut text = ptr::null_mut();
        // SAFETY: self contains a live SID and text is writable for the returned allocation.
        if unsafe { ConvertSidToStringSidW(self.raw(), &mut text) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let allocation = LocalAllocation(text.cast());
        let mut length = 0;
        // SAFETY: Windows returns a terminated UTF-16 SID string in this allocation.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: the preceding scan found length initialized UTF-16 units in allocation.
        let text = unsafe { std::slice::from_raw_parts(text, length) };
        let result = String::from_utf16(text).map_err(WindowsSecurityError::SidText);
        drop(allocation);
        result
    }
}

pub(super) fn descriptor(sddl: &str) -> Result<LocalAllocation> {
    let text: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: text is terminated UTF-16 and descriptor is writable for the returned LocalAlloc pointer.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(LocalAllocation(descriptor))
}

/// Windows permits bypassing directory traversal checks. The home and node
/// directory must therefore inherit no grants for other unprivileged accounts.
/// SYSTEM and administrators already control the process and are OS trust roots.
pub(super) fn require_private_file(file: &std::fs::File) -> Result<()> {
    let user = Sid::current()?;
    let mut owner = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: file remains open; all output pointers are valid, writable locals.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32).into());
    }
    let descriptor = LocalAllocation(descriptor);
    if owner.is_null() || !(user.matches_raw(owner) || privileged_sid(owner)) {
        return Err(WindowsSecurityError::UnexpectedOwner);
    }
    require_private_acl(&descriptor, &user)
}

fn require_private_acl(security: &LocalAllocation, user: &Sid) -> Result<()> {
    let mut present = 0;
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut defaulted = 0;
    // SAFETY: security owns a valid descriptor and the output pointers remain writable.
    if unsafe { GetSecurityDescriptorDacl(security.0, &mut present, &mut dacl, &mut defaulted) }
        == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    if present == 0 {
        return Err(WindowsSecurityError::DaclMissing);
    }
    // SAFETY: a non-null DACL was returned inside a live security descriptor.
    if dacl.is_null() || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(WindowsSecurityError::DaclInvalid);
    }
    // SAFETY: IsValidAcl established that dacl points to a valid ACL header.
    for index in 0..unsafe { (*dacl).AceCount } {
        let mut ace = ptr::null_mut();
        // SAFETY: index is within AceCount and ace is writable for the returned pointer.
        if unsafe { GetAce(dacl, u32::from(index), &mut ace) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: GetAce returned a valid ACE header in the live DACL.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if u32::from(header.AceType) == ACCESS_DENIED_ACE_TYPE {
            continue;
        }
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
            return Err(WindowsSecurityError::UnsupportedAce);
        }
        // SAFETY: this ACE has the validated ACCESS_ALLOWED_ACE layout and a valid trailing SID.
        let sid = unsafe {
            ptr::addr_of!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart)
                .cast_mut()
                .cast()
        };
        // SAFETY: sid is an ACE's valid SID; these checks only read it.
        let trusted = user.matches_raw(sid)
            || privileged_sid(sid)
            || unsafe {
                u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
                    && IsWellKnownSid(sid, WinCreatorOwnerSid) != 0
            };
        if !trusted {
            return Err(WindowsSecurityError::ForeignGrant);
        }
    }
    Ok(())
}

fn privileged_sid(sid: PSID) -> bool {
    // SAFETY: callers pass a valid SID in a live Windows security descriptor.
    unsafe {
        IsWellKnownSid(sid, WinLocalSystemSid) != 0
            || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    fn check_acl(sddl: &str) -> Result<()> {
        let security = descriptor(sddl)?;
        require_private_acl(&security, &Sid::current()?)
    }

    #[test]
    fn peer_token_matches_current_process_user() {
        // SAFETY: this function has no arguments and returns the current process ID.
        let peer = Sid::for_process(unsafe { GetCurrentProcessId() }).unwrap();
        let current = Sid::current().unwrap();
        assert!(peer.matches(&current));
        assert_eq!(peer.text().unwrap(), current.text().unwrap());
        assert!(Sid::for_process(u32::MAX).is_err());
    }

    #[test]
    fn private_acl_allows_only_the_user_and_os_administrators() {
        let user = Sid::current().unwrap().text().unwrap();
        assert!(check_acl(&format!(
            "D:P(A;;GA;;;{user})(A;;GA;;;SY)(A;;GA;;;BA)(A;OICIIO;GA;;;CO)"
        ))
        .is_ok());
        for account in ["WD", "AU", "BU", "AN"] {
            assert!(
                check_acl(&format!("D:P(A;;GA;;;{user})(A;;GR;;;{account})")).is_err(),
                "{account}"
            );
            assert!(
                check_acl(&format!("D:P(A;;GA;;;{user})(A;OICIIO;GR;;;{account})")).is_err(),
                "inherited {account}"
            );
        }
        assert!(check_acl(&format!("D:P(A;;GA;;;{user})(A;;GA;;;CO)")).is_err());
        assert!(check_acl("D:NO_ACCESS_CONTROL").is_err());
    }
}

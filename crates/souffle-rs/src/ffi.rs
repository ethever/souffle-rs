use std::{ffi::CString, ffi::c_int, slice, str};

use souffle_rs_sys::{SOUFFLE_RS_ABI_VERSION, SouffleRsError, SouffleRsStatus, SouffleRsString};
use strum::IntoStaticStr;

use crate::{AbiError, SouffleError};

/// Shared status/error translation for calls crossing the `souffle-rs` C ABI.
pub(crate) fn check_status(
    function: &'static str,
    status: c_int,
    error: &SouffleRsError,
) -> Result<(), SouffleError> {
    match FfiStatus::from_code(status) {
        FfiStatus::Ok => Ok(()),
        FfiStatus::Error => Err(AbiError::CallFailed {
            function: function.to_owned(),
            status: FfiStatus::Error.as_str().to_owned(),
            message: error_message(error)?,
        }
        .into()),
        FfiStatus::Exception => Err(SouffleError::CxxException {
            message: error_message(error)?,
        }),
        FfiStatus::AbiMismatch => Err(AbiError::CallFailed {
            function: function.to_owned(),
            status: FfiStatus::AbiMismatch.as_str().to_owned(),
            message: error_message(error)?,
        }
        .into()),
        FfiStatus::NullPointer => {
            let argument = error_message(error)?;
            Err(AbiError::NullPointer {
                argument: if argument.is_empty() {
                    function.to_owned()
                } else {
                    argument
                },
            }
            .into())
        }
        FfiStatus::CallbackFailed => Err(AbiError::CallbackFailed {
            message: error_message(error)?,
        }
        .into()),
        FfiStatus::Unknown(code) => Err(AbiError::UnknownErrorCode { code }.into()),
    }
}

/// Translate a status from a C ABI call, then release wrapper-owned error text.
pub(crate) fn check_owned_status(
    function: &'static str,
    status: c_int,
    error: &mut SouffleRsError,
) -> Result<(), SouffleError> {
    let result = check_status(function, status, error);
    // SAFETY: Embedded C ABI calls initialize `error` with either a null message
    // or wrapper-owned storage that must be released by the wrapper free hook.
    unsafe {
        souffle_rs_sys::souffle_rs_error_free(error as *mut SouffleRsError);
    }
    result
}

/// Verify that a loaded wrapper implements the ABI version expected by Rust.
pub(crate) fn check_abi_version(actual: u32) -> Result<(), SouffleError> {
    if actual == SOUFFLE_RS_ABI_VERSION {
        Ok(())
    } else {
        Err(AbiError::VersionMismatch {
            expected: SOUFFLE_RS_ABI_VERSION,
            actual,
        }
        .into())
    }
}

pub(crate) fn empty_error() -> SouffleRsError {
    SouffleRsError {
        status: SouffleRsStatus::Ok,
        message: SouffleRsString::null(),
    }
}

pub(crate) fn cstring_argument(
    argument: impl Into<String>,
    value: &str,
) -> Result<CString, SouffleError> {
    let argument = argument.into();
    CString::new(value).map_err(|source| {
        AbiError::InvalidString {
            argument,
            message: source.to_string(),
        }
        .into()
    })
}

fn error_message(error: &SouffleRsError) -> Result<String, SouffleError> {
    decode_abi_string(error.message, "SouffleRsError.message")
}

pub(crate) fn decode_abi_string(
    value: SouffleRsString,
    argument: &'static str,
) -> Result<String, SouffleError> {
    if value.len == 0 {
        return Ok(String::new());
    }
    if value.data.is_null() {
        return Err(AbiError::NullPointer {
            argument: argument.to_owned(),
        }
        .into());
    }

    // SAFETY: The C ABI pairs `data` with `len`; callers only borrow the bytes
    // for immediate Rust-owned materialization and never retain the pointer.
    let bytes = unsafe { slice::from_raw_parts(value.data.cast::<u8>(), value.len) };
    str::from_utf8(bytes).map(str::to_owned).map_err(|source| {
        AbiError::InvalidString {
            argument: argument.to_owned(),
            message: source.to_string(),
        }
        .into()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
enum FfiStatus {
    Ok,
    Error,
    Exception,
    AbiMismatch,
    NullPointer,
    CallbackFailed,
    Unknown(i32),
}

impl FfiStatus {
    fn from_code(code: c_int) -> Self {
        match code {
            0 => Self::Ok,
            1 => Self::Error,
            2 => Self::Exception,
            3 => Self::AbiMismatch,
            4 => Self::NullPointer,
            5 => Self::CallbackFailed,
            code => Self::Unknown(code),
        }
    }

    fn as_str(self) -> &'static str {
        self.into()
    }
}

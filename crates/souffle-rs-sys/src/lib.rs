//! Raw C ABI bindings for the C++ wrapper around Souffle generated code.
//!
//! This crate intentionally binds only the `souffle-rs` C ABI. It does not
//! expose Souffle C++ implementation types, C++ STL containers, templates, or
//! exceptions across the Rust boundary.
//!
//! Prefer the safe `souffle-rs` crate unless you are implementing or auditing
//! the wrapper boundary. Values passed into the ABI borrow Rust-owned buffers
//! for the duration of one call. Values returned by the wrapper are
//! wrapper-owned and must be released with the matching `*_free` function.
//!
//! # Example
//!
//! Construct the borrowed ABI shape for one row without calling into C++:
//!
//! ```
//! use std::ffi::CString;
//!
//! use souffle_rs_sys::{
//!     SouffleRsRow, SouffleRsValue, SouffleRsValueData, SouffleRsValueKind,
//! };
//!
//! let relation = CString::new("Input").unwrap();
//! let values = [SouffleRsValue {
//!     kind: SouffleRsValueKind::Number,
//!     as_: SouffleRsValueData { number: 7 },
//! }];
//! let row = SouffleRsRow {
//!     relation_name: relation.as_ptr(),
//!     values: values.as_ptr(),
//!     len: values.len(),
//!     composites: std::ptr::null(),
//!     composite_count: 0,
//! };
//!
//! assert_eq!(row.len, 1);
//! assert_eq!(values[0].kind, SouffleRsValueKind::Number);
//! ```

#![deny(missing_docs)]

use std::{
    ffi::{c_char, c_int, c_void},
    marker::{PhantomData, PhantomPinned},
};

use strum::IntoStaticStr;

/// ABI version expected by these Rust bindings.
pub const SOUFFLE_RS_ABI_VERSION: u32 = 5;

/// Status code returned by `souffle-rs` C ABI functions.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SouffleRsStatus {
    /// Operation completed successfully.
    Ok = 0,
    /// Operation failed with a typed wrapper error.
    Error = 1,
    /// C++ exception was caught before crossing the ABI boundary.
    Exception = 2,
    /// Loaded wrapper ABI version does not match the Rust bindings.
    AbiMismatch = 3,
    /// A required pointer argument was null.
    NullPointer = 4,
    /// A Rust callback reported failure to the C++ wrapper.
    CallbackFailed = 5,
}

/// Runtime kind of one ABI value.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum SouffleRsValueKind {
    /// Signed Souffle `number` value.
    Number = 0,
    /// Unsigned Souffle `unsigned` value.
    Unsigned = 1,
    /// Souffle `float` value encoded as IEEE-754 `double`.
    Float = 2,
    /// Souffle `symbol` value encoded as a borrowed string.
    Symbol = 3,
    /// Composite record value referenced through wrapper-owned storage.
    Record = 4,
    /// Composite list value referenced through wrapper-owned storage.
    List = 5,
    /// Composite ADT value referenced through wrapper-owned storage.
    Adt = 6,
    /// Nullary relation marker value.
    Nullary = 7,
}

/// Borrowed UTF-8 or byte string crossing the ABI.
///
/// The ABI string does not own bytes and is not guaranteed to be NUL
/// terminated. Callers must keep `data` alive for the ABI call that receives
/// it; wrapper-returned strings are valid only until the owning wrapper output
/// is freed.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SouffleRsString {
    /// Pointer to the first byte; null only when `len` is zero.
    pub data: *const c_char,
    /// Byte length, excluding any trailing NUL.
    pub len: usize,
}

impl SouffleRsString {
    /// Null string used for optional ABI fields.
    ///
    /// # Example
    ///
    /// ```
    /// use souffle_rs_sys::SouffleRsString;
    ///
    /// let string = SouffleRsString::null();
    ///
    /// assert!(string.data.is_null());
    /// assert_eq!(string.len, 0);
    /// ```
    pub const fn null() -> Self {
        Self {
            data: std::ptr::null(),
            len: 0,
        }
    }
}

/// Reference to wrapper-owned composite storage.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SouffleRsCompositeRef {
    /// Index into wrapper-owned composite storage.
    pub index: usize,
}

/// ABI value payload.
///
/// Read only the field selected by the surrounding [`SouffleRsValue::kind`].
#[repr(C)]
#[derive(Clone, Copy)]
pub union SouffleRsValueData {
    /// Payload for `SouffleRsValueKind::Number`.
    pub number: i64,
    /// Payload for `SouffleRsValueKind::Unsigned`.
    pub unsigned_value: u64,
    /// Payload for `SouffleRsValueKind::Float`.
    pub float_value: f64,
    /// Payload for `SouffleRsValueKind::Symbol`.
    pub symbol: SouffleRsString,
    /// Payload for composite value kinds.
    pub composite: SouffleRsCompositeRef,
}

/// One value in an ABI row.
///
/// Scalar values are stored directly in [`SouffleRsValueData`]. Composite
/// values store a [`SouffleRsCompositeRef`] index into the row or output's
/// composite storage.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SouffleRsValue {
    /// Discriminant selecting the active payload in `as_`.
    pub kind: SouffleRsValueKind,
    /// ABI payload corresponding to `kind`.
    pub as_: SouffleRsValueData,
}

/// Borrowed composite input value storage referenced by row values.
///
/// Input composites are caller-owned for the duration of one
/// `souffle_rs_program_insert_row` call. For ADTs, `variant` names the
/// constructor; for records and lists it must be [`SouffleRsString::null`].
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SouffleRsInputComposite {
    /// Composite kind: record, list, or ADT.
    pub kind: SouffleRsValueKind,
    /// Pointer to nested values owned by the caller for this ABI call.
    pub values: *const SouffleRsValue,
    /// Number of nested values.
    pub len: usize,
    /// ADT variant name; null for record and list composites.
    pub variant: SouffleRsString,
}

/// Borrowed row values passed to or returned from the wrapper.
///
/// Rows passed into the wrapper borrow caller-owned arrays and composite
/// storage. Rows returned by materialized or iterator output borrow
/// wrapper-owned storage released by the matching output free function.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SouffleRsRow {
    /// Borrowed relation name for insertion or returned output.
    pub relation_name: *const c_char,
    /// Borrowed row values in schema column order.
    pub values: *const SouffleRsValue,
    /// Number of row values.
    pub len: usize,
    /// Borrowed composite storage referenced by row values.
    pub composites: *const SouffleRsInputComposite,
    /// Number of entries in `composites`.
    pub composite_count: usize,
}

/// Run options passed to generated Souffle programs.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SouffleRsRunOptions {
    /// Explicit Souffle/OpenMP thread count requested by Rust.
    pub thread_count: usize,
}

/// Wrapper-owned materialized relation output.
///
/// Release successful outputs with [`souffle_rs_relation_output_free`]. The
/// `rows` pointer, nested row values, strings, and composite storage become
/// invalid after the free call.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SouffleRsRelationOutput {
    /// Relation name for the materialized rows.
    pub relation_name: *const c_char,
    /// Pointer to wrapper-owned row array.
    pub rows: *const SouffleRsRow,
    /// Number of rows in `rows`.
    pub len: usize,
    /// Opaque owner used by the wrapper to release row and composite buffers.
    pub owner: *mut c_void,
}

/// Wrapper-owned single-row iterator output.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SouffleRsRowOutput {
    /// Borrowed row returned from a wrapper-owned iterator.
    pub row: SouffleRsRow,
    /// Opaque owner used by the wrapper to release value and composite buffers.
    pub owner: *mut c_void,
}

/// Error payload filled by C ABI functions.
///
/// On failure, safe wrappers copy the message into Rust-owned memory and call
/// [`souffle_rs_error_free`]. Raw callers must follow the same ownership rule.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SouffleRsError {
    /// Status code associated with this error payload.
    pub status: SouffleRsStatus,
    /// Wrapper-owned human-readable diagnostic message.
    pub message: SouffleRsString,
}

/// Opaque generated program handle.
#[repr(C)]
pub struct SouffleRsProgram {
    _private: [u8; 0],
    _pinned: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Opaque iterator handle for streaming relation rows.
#[repr(C)]
pub struct SouffleRsRelationIterator {
    _private: [u8; 0],
    _pinned: PhantomData<(*mut u8, PhantomPinned)>,
}

/// Callback invoked by the wrapper for streaming relation rows.
///
/// The callback must return a `SouffleRsStatus` code and must not unwind across
/// the C ABI boundary.
pub type SouffleRsRowCallback =
    Option<unsafe extern "C" fn(row: *const SouffleRsRow, user_data: *mut c_void) -> c_int>;

unsafe extern "C" {
    /// Return the ABI version implemented by the loaded wrapper.
    pub fn souffle_rs_abi_version() -> u32;

    /// Create a generated program instance.
    pub fn souffle_rs_program_new(
        program_name: *const c_char,
        program_output: *mut *mut SouffleRsProgram,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Set the generated program's Souffle thread count.
    pub fn souffle_rs_program_set_threads(
        program: *mut SouffleRsProgram,
        thread_count: usize,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Insert one row into a loadable relation.
    pub fn souffle_rs_program_insert_row(
        program: *mut SouffleRsProgram,
        row: *const SouffleRsRow,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Execute the generated program.
    pub fn souffle_rs_program_run(
        program: *mut SouffleRsProgram,
        options: *const SouffleRsRunOptions,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Materialize a printable relation.
    pub fn souffle_rs_program_read_relation(
        program: *mut SouffleRsProgram,
        relation_name: *const c_char,
        relation_output: *mut SouffleRsRelationOutput,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Open a wrapper-owned streaming iterator for a printable relation.
    pub fn souffle_rs_program_open_relation_iterator(
        program: *mut SouffleRsProgram,
        relation_name: *const c_char,
        iterator_output: *mut *mut SouffleRsRelationIterator,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Pull at most one row from a wrapper-owned relation iterator.
    ///
    /// The returned row output is populated only when `has_row_output` is
    /// non-zero and must be freed with `souffle_rs_row_output_free`.
    pub fn souffle_rs_relation_iterator_next(
        iterator: *mut SouffleRsRelationIterator,
        has_row_output: *mut u8,
        row_output: *mut SouffleRsRowOutput,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Pull a bounded chunk from a wrapper-owned relation iterator.
    pub fn souffle_rs_relation_iterator_next_chunk(
        iterator: *mut SouffleRsRelationIterator,
        max_rows: usize,
        has_rows_output: *mut u8,
        relation_output: *mut SouffleRsRelationOutput,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Stream rows from a printable relation through a callback.
    pub fn souffle_rs_program_for_each_row(
        program: *mut SouffleRsProgram,
        relation_name: *const c_char,
        callback: SouffleRsRowCallback,
        user_data: *mut c_void,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Return the number of fields/elements in a wrapper-owned composite value.
    pub fn souffle_rs_relation_output_composite_len(
        relation_output: *const SouffleRsRelationOutput,
        composite: SouffleRsCompositeRef,
        len_output: *mut usize,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Return one field/element from a wrapper-owned composite value.
    pub fn souffle_rs_relation_output_composite_value(
        relation_output: *const SouffleRsRelationOutput,
        composite: SouffleRsCompositeRef,
        index: usize,
        value_output: *mut SouffleRsValue,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Return the variant name for a wrapper-owned ADT composite value.
    pub fn souffle_rs_relation_output_adt_variant(
        relation_output: *const SouffleRsRelationOutput,
        composite: SouffleRsCompositeRef,
        variant_output: *mut SouffleRsString,
        error: *mut SouffleRsError,
    ) -> c_int;

    /// Free a generated program instance.
    pub fn souffle_rs_program_free(program: *mut SouffleRsProgram);

    /// Free a wrapper-owned relation iterator.
    pub fn souffle_rs_relation_iterator_free(iterator: *mut SouffleRsRelationIterator);

    /// Free wrapper-owned single-row output and its buffers.
    pub fn souffle_rs_row_output_free(row_output: *mut SouffleRsRowOutput);

    /// Free materialized relation output and its wrapper-owned buffers.
    pub fn souffle_rs_relation_output_free(relation_output: *mut SouffleRsRelationOutput);

    /// Free wrapper-owned error storage.
    pub fn souffle_rs_error_free(error: *mut SouffleRsError);
}

#[cfg(test)]
mod tests {
    use std::mem::{align_of, size_of};

    use super::*;

    #[test]
    fn abi_version_is_explicit() {
        assert_eq!(SOUFFLE_RS_ABI_VERSION, 5);
    }

    #[test]
    fn c_structs_have_stable_field_sizes() {
        assert_eq!(
            size_of::<SouffleRsString>(),
            size_of::<*const c_char>() + size_of::<usize>()
        );
        assert!(align_of::<SouffleRsValue>() >= align_of::<SouffleRsValueData>());
        assert_eq!(size_of::<SouffleRsCompositeRef>(), size_of::<usize>());
        assert_eq!(size_of::<SouffleRsRunOptions>(), size_of::<usize>());
        assert_eq!(
            size_of::<SouffleRsRelationOutput>(),
            (size_of::<*const c_char>() * 2) + size_of::<usize>() + size_of::<*mut c_void>()
        );
        assert_eq!(
            size_of::<SouffleRsRowOutput>(),
            size_of::<SouffleRsRow>() + size_of::<*mut c_void>()
        );
        assert_eq!(
            size_of::<SouffleRsInputComposite>(),
            size_of::<SouffleRsValueKind>()
                + (size_of::<usize>() - size_of::<SouffleRsValueKind>())
                + size_of::<*const SouffleRsValue>()
                + size_of::<usize>()
                + size_of::<SouffleRsString>()
        );
    }
}

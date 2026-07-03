mod decode;
mod encode;
mod iterator;

use std::{ffi::CString, num::NonZeroUsize, ptr::NonNull};

use souffle_rs_sys::{
    SouffleRsError, SouffleRsProgram, SouffleRsRelationOutput, SouffleRsRow, SouffleRsRunOptions,
};

use crate::{
    AbiError, Backend, BuildInfo, Program, ProgramBuilder, ProgramConfig, RelationBundle,
    RelationIterator, RelationOutput, Row, RunOptions, SouffleError, ffi,
};

use decode::decode_relation_output;
#[cfg(test)]
pub(crate) use decode::decode_scalar_value;
pub(crate) use encode::encode_input_row;
pub(crate) use iterator::EmbeddedRelationIterator;

/// In-process handle for a generated Souffle program through the `souffle-rs`
/// C ABI.
///
/// `EmbeddedProgram` expects the generated C++ wrapper symbols to be linked
/// into the current Rust binary. In normal use, `souffle-rs-build` emits the
/// C ABI wrapper, compiles the generated C++, and records the schema bundle
/// used here for validation and decoding.
///
/// # Example
///
/// ```no_run
/// use souffle_rs::{
///     AttributeSchema, EmbeddedProgram, Program, RelationBundle, RelationId,
///     RelationSchema, TypeRef, Value,
/// };
///
/// # fn main() -> Result<(), souffle_rs::SouffleError> {
/// let schema: RelationBundle = [
///     RelationSchema::input(
///         RelationId::new(0),
///         "Input",
///         [AttributeSchema::new("id", TypeRef::Number)],
///     ),
///     RelationSchema::output(
///         RelationId::new(1),
///         "Output",
///         [AttributeSchema::new("id", TypeRef::Number)],
///     ),
/// ]
/// .into_iter()
/// .collect();
/// let mut program = EmbeddedProgram::builder("analysis")
///     .schema(schema)
///     .build_embedded()?;
///
/// program.insert_row("Input", [Value::Number(7)])?;
/// program.run()?;
/// let _rows = program.read_relation("Output")?;
/// # Ok(())
/// # }
/// ```
pub struct EmbeddedProgram {
    config: ProgramConfig,
    schema: RelationBundle,
    handle: NonNull<SouffleRsProgram>,
}

impl EmbeddedProgram {
    /// Start building an embedded generated-program facade.
    pub fn builder(name: impl Into<String>) -> ProgramBuilder {
        ProgramBuilder::new(name).backend(Backend::Embedded)
    }

    /// Create a generated program instance through the loaded C ABI wrapper.
    ///
    /// This validates the ABI version before asking the wrapper for a program
    /// handle. Row insertion, run, and output materialization are layered on top
    /// of this lifecycle boundary.
    pub fn new(name: impl Into<String>, schema: RelationBundle) -> Result<Self, SouffleError> {
        Self::from_config(
            ProgramConfig::new(name).with_backend(Backend::Embedded),
            schema,
        )
    }

    pub(crate) fn from_config(
        config: ProgramConfig,
        schema: RelationBundle,
    ) -> Result<Self, SouffleError> {
        schema.validate()?;
        let program_name = program_name_cstring(config.name())?;
        let mut error = ffi::empty_error();

        // SAFETY: The wrapper exposes this nullary function as part of the C
        // ABI and does not retain Rust-owned memory.
        let actual_abi_version = unsafe { souffle_rs_sys::souffle_rs_abi_version() };
        ffi::check_abi_version(actual_abi_version)?;

        let mut handle = std::ptr::null_mut();
        // SAFETY: `program_name` lives for the call, `handle` is a valid output
        // pointer, and `error` is wrapper-owned only for the duration of the
        // call. `check_owned_status` copies any error message into Rust-owned
        // memory before freeing wrapper-owned storage.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_program_new(
                program_name.as_ptr(),
                &mut handle,
                &mut error as *mut SouffleRsError,
            )
        };
        ffi::check_owned_status("souffle_rs_program_new", status, &mut error)?;

        let handle = NonNull::new(handle).ok_or_else(|| {
            SouffleError::Abi(AbiError::NullPointer {
                argument: "program_output".to_owned(),
            })
        })?;

        let program = Self {
            config: config.with_backend(Backend::Embedded),
            schema,
            handle,
        };
        program.set_threads(program.config.cpu_budget().souffle_threads())?;
        Ok(program)
    }

    /// Generated program name.
    pub fn name(&self) -> &str {
        self.config.name()
    }

    /// Runtime-visible build and ABI metadata for the embedded handle.
    pub fn build_info(&self) -> Result<BuildInfo, SouffleError> {
        Ok(BuildInfo::new(
            self.name(),
            Backend::Embedded,
            self.abi_version()?,
            self.schema.clone(),
        ))
    }

    /// C ABI version implemented by the loaded wrapper.
    pub fn abi_version(&self) -> Result<u32, SouffleError> {
        // SAFETY: The wrapper exposes this nullary function as part of the C ABI.
        let version = unsafe { souffle_rs_sys::souffle_rs_abi_version() };
        ffi::check_abi_version(version)?;
        Ok(version)
    }

    fn set_threads(&self, threads: NonZeroUsize) -> Result<(), SouffleError> {
        let mut error = ffi::empty_error();
        // SAFETY: `handle` is a live generated program owned by `self`, and the
        // C ABI reads only the scalar thread count for the duration of the call.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_program_set_threads(
                self.handle.as_ptr(),
                threads.get(),
                &mut error as *mut SouffleRsError,
            )
        };
        ffi::check_owned_status("souffle_rs_program_set_threads", status, &mut error)
    }
}

impl Program for EmbeddedProgram {
    fn name(&self) -> &str {
        self.config.name()
    }

    fn backend(&self) -> Backend {
        Backend::Embedded
    }

    fn schema_bundle(&self) -> &RelationBundle {
        &self.schema
    }

    fn abi_version(&self) -> Result<u32, SouffleError> {
        Self::abi_version(self)
    }

    fn build_info(&self) -> Result<BuildInfo, SouffleError> {
        Self::build_info(self)
    }

    fn insert_row(&mut self, relation: &str, row: impl Into<Row>) -> Result<(), SouffleError> {
        let schema = self.relation_schema(relation)?;
        if !schema.is_loadable() {
            return Err(SouffleError::RelationNotInput {
                relation: relation.to_owned(),
            });
        }

        let row = row.into();
        let encoded = encode_input_row(schema, &row)?;
        let abi_row = encoded.as_ffi();
        let mut error = ffi::empty_error();

        // SAFETY: `abi_row` borrows buffers owned by `encoded`, all of which
        // live until the call returns. The wrapper must not retain borrowed Rust
        // pointers across the ABI boundary.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_program_insert_row(
                self.handle.as_ptr(),
                &abi_row as *const SouffleRsRow,
                &mut error as *mut SouffleRsError,
            )
        };
        ffi::check_owned_status("souffle_rs_program_insert_row", status, &mut error)
    }

    fn run_with_options(&mut self, options: RunOptions) -> Result<(), SouffleError> {
        self.set_threads(options.threads())?;
        let options = SouffleRsRunOptions {
            thread_count: options.threads().get(),
        };
        let mut error = ffi::empty_error();

        // SAFETY: `handle` is a live generated program and `options` is a
        // borrowed POD struct read only during the call.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_program_run(
                self.handle.as_ptr(),
                &options as *const SouffleRsRunOptions,
                &mut error as *mut SouffleRsError,
            )
        };
        ffi::check_owned_status("souffle_rs_program_run", status, &mut error)
    }

    fn default_run_options(&self) -> RunOptions {
        RunOptions::from_cpu_budget(self.config.cpu_budget())
    }

    fn iter_relation<'program>(
        &'program self,
        relation: &str,
    ) -> Result<RelationIterator<'program>, SouffleError> {
        let schema = self.relation_schema(relation)?;
        if !schema.is_printable() {
            return Err(SouffleError::RelationNotOutput {
                relation: relation.to_owned(),
            });
        }
        let iterator = EmbeddedRelationIterator::open(self, schema)?;
        Ok(RelationIterator::from_embedded(schema.clone(), iterator))
    }

    fn read_relation(&self, relation: &str) -> Result<RelationOutput, SouffleError> {
        let schema = self.relation_schema(relation)?;
        if !schema.is_printable() {
            return Err(SouffleError::RelationNotOutput {
                relation: relation.to_owned(),
            });
        }

        let relation_name = relation_cstring(relation)?;
        let mut raw_output = SouffleRsRelationOutput {
            relation_name: std::ptr::null(),
            rows: std::ptr::null(),
            len: 0,
            owner: std::ptr::null_mut(),
        };
        let mut error = ffi::empty_error();

        // SAFETY: `relation_name` and `raw_output` live for the call. On
        // success, `raw_output` owns wrapper-allocated buffers released by
        // `MaterializedOutput`.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_program_read_relation(
                self.handle.as_ptr(),
                relation_name.as_ptr(),
                &mut raw_output as *mut SouffleRsRelationOutput,
                &mut error as *mut SouffleRsError,
            )
        };
        ffi::check_owned_status("souffle_rs_program_read_relation", status, &mut error)?;

        let rows = decode_relation_output(schema, raw_output)?;
        RelationOutput::new(schema.clone(), rows)
    }
}

impl Drop for EmbeddedProgram {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by this `EmbeddedProgram` and is freed
        // exactly once at drop.
        unsafe {
            souffle_rs_sys::souffle_rs_program_free(self.handle.as_ptr());
        }
    }
}

pub(crate) fn program_name_cstring(name: &str) -> Result<CString, SouffleError> {
    ffi::cstring_argument("program_name", name)
}

pub(crate) fn relation_cstring(relation: &str) -> Result<CString, SouffleError> {
    ffi::cstring_argument("relation_name", relation)
}

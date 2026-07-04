use std::{marker::PhantomData, ptr::NonNull};

use souffle_rs_sys::{
    SouffleRsError, SouffleRsRelationIterator, SouffleRsRelationOutput, SouffleRsRow,
    SouffleRsRowOutput,
};

use crate::{AbiError, RelationSchema, Row, SouffleError, ffi, program::RelationIteratorSource};

use super::{
    EmbeddedProgram,
    decode::{decode_relation_output, decode_row_output},
    relation_cstring,
};

/// Safe owner for the generated wrapper's opaque relation iterator handle.
#[derive(Debug)]
pub(crate) struct EmbeddedRelationIterator<'program> {
    handle: NonNull<SouffleRsRelationIterator>,
    _program: PhantomData<&'program EmbeddedProgram>,
}

impl<'program> EmbeddedRelationIterator<'program> {
    pub(crate) fn open(
        program: &'program EmbeddedProgram,
        schema: &RelationSchema,
    ) -> Result<Self, SouffleError> {
        let relation_name = relation_cstring(schema.name())?;
        let mut error = ffi::empty_error();
        let mut handle = std::ptr::null_mut();

        // SAFETY: `program` owns a live generated C++ handle, `relation_name`
        // lives for the call, and the wrapper writes an opaque iterator handle
        // that is freed by this Rust owner.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_program_open_relation_iterator(
                program.handle.as_ptr(),
                relation_name.as_ptr(),
                &mut handle,
                &mut error as *mut SouffleRsError,
            )
        };
        ffi::check_owned_status(
            "souffle_rs_program_open_relation_iterator",
            status,
            &mut error,
        )?;

        let handle = NonNull::new(handle).ok_or_else(|| {
            SouffleError::Abi(AbiError::NullPointer {
                argument: "iterator_output".to_owned(),
            })
        })?;

        Ok(Self {
            handle,
            _program: PhantomData,
        })
    }

    fn next_owned_row(&mut self, schema: &RelationSchema) -> Result<Option<Row>, SouffleError> {
        let mut has_row = 0u8;
        let mut raw_output = empty_row_output();
        let mut error = ffi::empty_error();

        // SAFETY: `handle` is owned by this iterator, `has_row` and
        // `raw_output` are valid output pointers for the duration of the call.
        // When a row is produced, `raw_output` owns wrapper buffers that are
        // released by `decode_row_output`.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_relation_iterator_next(
                self.handle.as_ptr(),
                &mut has_row,
                &mut raw_output as *mut SouffleRsRowOutput,
                &mut error as *mut SouffleRsError,
            )
        };
        let status_result =
            ffi::check_owned_status("souffle_rs_relation_iterator_next", status, &mut error);
        if let Err(error) = status_result {
            free_row_output(&mut raw_output);
            return Err(error);
        }

        if has_row == 0 {
            free_row_output(&mut raw_output);
            return Ok(None);
        }

        Ok(Some(decode_row_output(schema, raw_output)?))
    }

    pub(crate) fn next_owned_chunk(
        &mut self,
        schema: &RelationSchema,
        max_rows: usize,
    ) -> Result<Vec<Row>, SouffleError> {
        if max_rows == 0 {
            return Ok(Vec::new());
        }

        let mut has_rows = 0u8;
        let mut raw_output = empty_relation_output();
        let mut error = ffi::empty_error();

        // SAFETY: `handle` is owned by this iterator, `has_rows` and
        // `raw_output` are valid output pointers for the duration of the call.
        // When rows are produced, `raw_output` owns wrapper buffers that are
        // released by `decode_relation_output`.
        let status = unsafe {
            souffle_rs_sys::souffle_rs_relation_iterator_next_chunk(
                self.handle.as_ptr(),
                max_rows,
                &mut has_rows,
                &mut raw_output as *mut SouffleRsRelationOutput,
                &mut error as *mut SouffleRsError,
            )
        };
        let status_result = ffi::check_owned_status(
            "souffle_rs_relation_iterator_next_chunk",
            status,
            &mut error,
        );
        if let Err(error) = status_result {
            free_relation_output(&mut raw_output);
            return Err(error);
        }

        if has_rows == 0 {
            free_relation_output(&mut raw_output);
            return Ok(Vec::new());
        }

        let raw_len = raw_output.len;
        let rows = decode_relation_output(schema, raw_output)?;
        if raw_len == 0 || rows.is_empty() {
            return Err(AbiError::CallFailed {
                function: "souffle_rs_relation_iterator_next_chunk".to_owned(),
                status: "error".to_owned(),
                message: format!(
                    "expected at least one iterator row but wrapper returned {raw_len}"
                ),
            }
            .into());
        }
        if raw_len > max_rows {
            return Err(AbiError::CallFailed {
                function: "souffle_rs_relation_iterator_next_chunk".to_owned(),
                status: "error".to_owned(),
                message: format!(
                    "expected at most {max_rows} iterator rows but wrapper returned {raw_len}"
                ),
            }
            .into());
        }

        Ok(rows)
    }
}

impl RelationIteratorSource for EmbeddedRelationIterator<'_> {
    fn next_row(&mut self, schema: &RelationSchema) -> Result<Option<Row>, SouffleError> {
        self.next_owned_row(schema)
    }

    fn next_chunk(
        &mut self,
        schema: &RelationSchema,
        max_rows: usize,
    ) -> Result<Vec<Row>, SouffleError> {
        self.next_owned_chunk(schema, max_rows)
    }
}

impl Drop for EmbeddedRelationIterator<'_> {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by this iterator and is freed exactly once.
        unsafe {
            souffle_rs_sys::souffle_rs_relation_iterator_free(self.handle.as_ptr());
        }
    }
}

fn empty_relation_output() -> SouffleRsRelationOutput {
    SouffleRsRelationOutput {
        relation_name: std::ptr::null(),
        rows: std::ptr::null(),
        len: 0,
        owner: std::ptr::null_mut(),
    }
}

fn empty_row_output() -> SouffleRsRowOutput {
    SouffleRsRowOutput {
        row: SouffleRsRow {
            relation_name: std::ptr::null(),
            values: std::ptr::null(),
            len: 0,
            composites: std::ptr::null(),
            composite_count: 0,
        },
        owner: std::ptr::null_mut(),
    }
}

fn free_row_output(output: &mut SouffleRsRowOutput) {
    // SAFETY: The C ABI free hook accepts empty outputs and clears the struct.
    unsafe {
        souffle_rs_sys::souffle_rs_row_output_free(output as *mut SouffleRsRowOutput);
    }
}

fn free_relation_output(output: &mut SouffleRsRelationOutput) {
    // SAFETY: The C ABI free hook accepts empty outputs and clears the struct.
    unsafe {
        souffle_rs_sys::souffle_rs_relation_output_free(output as *mut SouffleRsRelationOutput);
    }
}

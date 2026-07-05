use std::{fs, num::NonZeroUsize, time::Duration};

use crate::{
    AttributeSchema, Backend, CpuBudget, FileProgram, FileRelationStore, InMemoryProgram,
    PerformanceRecorder, ProcessConfig, ProcessProgram, Program, ProgramConfig, RelationBundle,
    RelationHandle, RelationId, RelationIterator, RelationKind, RelationOutput, RelationSchema,
    Row, RunOptions, SouffleError, TypeRef, Value, ValueKind,
    embedded::program_name_cstring,
    embedded::{decode_scalar_value, encode_input_row},
    ffi::{check_abi_version, check_status},
    verify_backend_parity,
};
#[cfg(feature = "sqlite")]
use crate::{SqliteProgram, SqliteRelationStore};
use souffle_rs_sys::{
    SouffleRsError, SouffleRsStatus, SouffleRsString, SouffleRsValue, SouffleRsValueData,
    SouffleRsValueKind,
};

fn sample_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new("label", TypeRef::Symbol),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new(
                    "payload",
                    TypeRef::Record(vec![TypeRef::Unsigned, TypeRef::Float]),
                ),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

fn scalar_process_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new("label", TypeRef::Symbol),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new("label", TypeRef::Symbol),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

#[derive(Debug, Clone)]
struct IteratorOnlyProgram {
    schema: RelationBundle,
    output_rows: Vec<Row>,
}

impl IteratorOnlyProgram {
    fn new(output_rows: Vec<Row>) -> Self {
        Self {
            schema: sample_schema(),
            output_rows,
        }
    }
}

impl Program for IteratorOnlyProgram {
    fn name(&self) -> &str {
        "analysis"
    }

    fn backend(&self) -> Backend {
        Backend::Memory
    }

    fn schema_bundle(&self) -> &RelationBundle {
        &self.schema
    }

    fn abi_version(&self) -> Result<u32, SouffleError> {
        Ok(souffle_rs_sys::SOUFFLE_RS_ABI_VERSION)
    }

    fn insert_row(&mut self, relation: &str, _row: impl Into<Row>) -> Result<(), SouffleError> {
        Err(SouffleError::RelationNotInput {
            relation: relation.to_owned(),
        })
    }

    fn run_with_options(&mut self, _options: RunOptions) -> Result<(), SouffleError> {
        Ok(())
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
        Ok(RelationIterator::new(
            schema.clone(),
            self.output_rows.clone(),
        ))
    }

    fn read_relation(&self, _relation: &str) -> Result<RelationOutput, SouffleError> {
        panic!("streaming path must not materialize through read_relation")
    }
}

#[test]
fn in_memory_program_validates_dynamic_rows() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();

    program
        .insert_row("Input", [Value::Number(1), Value::Symbol("entry".into())])
        .unwrap();

    assert_eq!(program.name(), "analysis");
    assert_eq!(program.backend(), Backend::Memory);
    assert_eq!(program.relation_schema("Input").unwrap().arity(), 2);
}

#[test]
fn build_info_exposes_schema_backend_and_abi_version() {
    let program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();

    let build_info = program.build_info().unwrap();

    assert_eq!(build_info.program(), "analysis");
    assert_eq!(build_info.backend(), Backend::Memory);
    assert_eq!(
        build_info.abi_version(),
        souffle_rs_sys::SOUFFLE_RS_ABI_VERSION
    );
    assert_eq!(build_info.schema_bundle(), program.schema_bundle());
    assert_eq!(program.abi_version().unwrap(), build_info.abi_version());
}

#[test]
fn performance_recorder_tracks_memory_backend_file_free_exchange() {
    let cpu_budget = CpuBudget::new(NonZeroUsize::new(2).unwrap(), NonZeroUsize::new(4).unwrap());
    let mut program = InMemoryProgram::builder("analysis")
        .cpu_budget(cpu_budget.clone())
        .schema(sample_schema())
        .build_memory();
    let mut recorder = PerformanceRecorder::new(program.backend(), &cpu_budget);

    recorder
        .measure_relation_insertion(|| {
            program.insert_row("Input", [Value::Number(1), Value::Symbol("entry".into())])
        })
        .unwrap();
    program
        .replace_relation_rows(
            "Output",
            [Row::new([
                Value::Number(1),
                Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
            ])],
        )
        .unwrap();
    recorder.measure_souffle_run(|| program.run()).unwrap();
    let decoded_rows = recorder
        .measure_output_decode(|| {
            let mut rows = program.iter_relation("Output")?;
            let mut decoded = 0usize;
            while rows.next_row()?.is_some() {
                decoded += 1;
            }
            Ok::<_, SouffleError>(decoded)
        })
        .unwrap();

    assert_eq!(decoded_rows, 1);
    let metrics = recorder.finish();
    assert_eq!(metrics.backend(), Backend::Memory);
    assert_eq!(metrics.openmp_threads(), 4);
    assert_eq!(metrics.rust_worker_count(), 2);
    assert_eq!(metrics.file_count(), 0);
    assert_eq!(metrics.bytes_written(), 0);
    assert_eq!(metrics.metadata_operations(), 0);
    assert!(metrics.relation_exchange_is_file_free());
    assert!(metrics.total_time() >= metrics.souffle_run_time());
    assert!(metrics.total_time() >= metrics.relation_insertion_time());
    assert!(metrics.total_time() >= metrics.relation_output_decode_time());

    let json = serde_json::to_string(&metrics).unwrap();
    assert!(json.contains("\"backend\":\"memory\""));
    assert!(json.contains("\"file_count\":0"));
    assert!(json.contains("\"bytes_written\":0"));
    assert!(json.contains("\"metadata_operations\":0"));
    assert!(json.contains("\"openmp_threads\":4"));
    assert!(json.contains("\"rust_worker_count\":2"));
}

#[test]
fn performance_recorder_counts_file_backend_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = FileRelationStore::new(tempdir.path().join("relations"));
    let mut program = FileProgram::builder("analysis")
        .schema(sample_schema())
        .file_store(store.clone())
        .build_file()
        .unwrap();
    program
        .replace_relation_rows(
            "Output",
            [Row::new([
                Value::Number(1),
                Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
            ])],
        )
        .unwrap();

    let mut recorder = PerformanceRecorder::new(Backend::File, &CpuBudget::default());
    recorder.record_artifact_path(store.root()).unwrap();
    let metrics = recorder.finish();

    assert_eq!(metrics.backend(), Backend::File);
    assert!(metrics.file_count() >= 3);
    assert!(metrics.bytes_written() > 0);
    assert!(metrics.metadata_operations() >= metrics.file_count());
    assert!(!metrics.relation_exchange_is_file_free());
}

#[test]
#[cfg(feature = "sqlite")]
fn performance_recorder_counts_sqlite_backend_artifacts() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = SqliteRelationStore::new(tempdir.path().join("relations.duckdb"));
    let mut program = SqliteProgram::builder("analysis")
        .schema(sample_schema())
        .sqlite_store(store.clone())
        .build_sqlite()
        .unwrap();
    program
        .replace_relation_rows(
            "Output",
            [Row::new([
                Value::Number(1),
                Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
            ])],
        )
        .unwrap();

    let mut recorder = PerformanceRecorder::new(Backend::Sqlite, &CpuBudget::default());
    recorder.record_artifact_path(store.path()).unwrap();
    let metrics = recorder.finish();

    assert_eq!(metrics.backend(), Backend::Sqlite);
    assert_eq!(metrics.file_count(), 1);
    assert!(metrics.bytes_written() > 0);
    assert!(metrics.metadata_operations() >= 1);
    assert!(!metrics.relation_exchange_is_file_free());
}

#[test]
fn performance_recorder_samples_or_overrides_resource_metrics() {
    let mut recorder = PerformanceRecorder::new(Backend::Memory, &CpuBudget::default());
    recorder.measure_souffle_run(|| {
        let mut accumulator = 0u64;
        for value in 0..50_000 {
            accumulator = accumulator.wrapping_add(value);
        }
        std::hint::black_box(accumulator);
    });
    let metrics = recorder.finish();

    if cfg!(target_os = "linux") {
        assert!(metrics.peak_rss_bytes().unwrap_or_default() > 0);
        assert!(metrics.cpu_utilization().is_some());
    }

    let mut recorder = PerformanceRecorder::new(Backend::Memory, &CpuBudget::default());
    recorder.set_peak_rss_bytes(123_456);
    recorder.set_cpu_utilization(0.75);
    let metrics = recorder.finish();

    assert_eq!(metrics.peak_rss_bytes(), Some(123_456));
    assert_eq!(metrics.cpu_utilization(), Some(0.75));
}

#[test]
fn schema_exposes_normalized_runtime_value_types() {
    let union = TypeRef::Union {
        name: "Bucket".to_owned(),
        variants: vec![
            TypeRef::Subtype {
                name: "Small".to_owned(),
                base: Box::new(TypeRef::Number),
            },
            TypeRef::Declared {
                name: "Label".to_owned(),
                runtime: Box::new(TypeRef::Symbol),
            },
        ],
    };
    let record = TypeRef::Record(vec![TypeRef::Unsigned, TypeRef::Float]);
    let adt = TypeRef::adt("Choice", [("Some".to_owned(), vec![TypeRef::Number])]);

    assert_eq!(
        union.runtime_value_kinds(),
        vec![ValueKind::Number, ValueKind::Symbol]
    );
    assert_eq!(record.runtime_value_kinds(), vec![ValueKind::Record]);
    assert_eq!(adt.runtime_value_kinds(), vec![ValueKind::Adt]);

    let schema: RelationBundle = [RelationSchema::output(
        RelationId::new(0),
        "Output",
        [
            AttributeSchema::new("bucket", union),
            AttributeSchema::new("payload", record),
            AttributeSchema::new("choice", adt),
        ],
    )]
    .into_iter()
    .collect();
    let relation = schema.get("Output").unwrap();

    assert_eq!(
        relation.attributes()[0].runtime_types(),
        vec![ValueKind::Number, ValueKind::Symbol]
    );
    assert_eq!(
        relation.attributes()[1].runtime_types(),
        vec![ValueKind::Record]
    );
    assert_eq!(
        relation.attributes()[2].runtime_types(),
        vec![ValueKind::Adt]
    );

    let json = serde_json::to_string(&schema).unwrap();
    assert!(json.contains("runtime_types"));
    assert!(json.contains("number"));
    assert!(json.contains("symbol"));
    assert!(json.contains("record"));
    assert!(json.contains("adt"));

    let decoded: RelationBundle = serde_json::from_str(&json).unwrap();
    assert_eq!(
        decoded.get("Output").unwrap().attributes()[0].runtime_types(),
        vec![ValueKind::Number, ValueKind::Symbol]
    );
}

#[test]
fn relation_bundle_from_json_str_validates_decoded_schema() {
    let schema = sample_schema();
    let json = serde_json::to_string(&schema).unwrap();

    let decoded = RelationBundle::from_json_str(&json).unwrap();

    assert_eq!(decoded, schema);

    let invalid_json = r#"{"Input":{"id":0,"name":"Input","kind":"input","attributes":[{"name":"choice","declared_type":{"adt":{"name":"Choice","variants":{"Some":["number"]},"variant_order":[],"is_enum":false}},"runtime_types":["adt"]}],"loadable":true,"printable":false}}"#;
    let error = RelationBundle::from_json_str(invalid_json).unwrap_err();
    assert!(matches!(error, SouffleError::SchemaValidation { .. }));
}

#[test]
fn schema_preserves_declared_identity_for_typed_subtypes_and_unions() {
    let small = TypeRef::Subtype {
        name: "Small".to_owned(),
        base: Box::new(TypeRef::Number),
    };
    let large = TypeRef::Subtype {
        name: "Large".to_owned(),
        base: Box::new(TypeRef::Number),
    };
    let bucket = TypeRef::Union {
        name: "Bucket".to_owned(),
        variants: vec![small.clone(), large],
    };
    let schema = RelationBundle::from_iter([
        RelationSchema::input(
            RelationId::new(0),
            "SmallIn",
            [AttributeSchema::new("value", small)],
        ),
        RelationSchema::input(
            RelationId::new(1),
            "BucketIn",
            [AttributeSchema::new("value", bucket)],
        ),
    ]);
    let mut program = InMemoryProgram::builder("analysis")
        .schema(schema)
        .build_memory();

    program
        .insert_row("SmallIn", [Value::Number(3)])
        .expect("raw base values remain compatible");
    program
        .insert_row("SmallIn", [Value::typed("Small", Value::Number(5))])
        .expect("matching subtype wrapper is accepted");
    program
        .insert_row("BucketIn", [Value::typed("Small", Value::Number(7))])
        .expect("union accepts an explicitly typed variant");
    program
        .insert_row("BucketIn", [Value::typed("Bucket", Value::Number(11))])
        .expect("union accepts its own declared wrapper");
    program
        .insert_row(
            "BucketIn",
            [Value::typed(
                "Bucket",
                Value::typed("Large", Value::Number(12)),
            )],
        )
        .expect("union accepts a nested explicitly typed variant");

    let error = program
        .insert_row("SmallIn", [Value::typed("Large", Value::Number(13))])
        .unwrap_err();
    assert_eq!(
        error,
        SouffleError::TypeMismatch {
            relation: "SmallIn".to_owned(),
            column: "value".to_owned(),
            expected: "Small".to_owned(),
            actual: "Large".to_owned(),
        }
    );

    let error = program
        .insert_row("BucketIn", [Value::typed("Other", Value::Number(17))])
        .unwrap_err();
    assert_eq!(
        error,
        SouffleError::TypeMismatch {
            relation: "BucketIn".to_owned(),
            column: "value".to_owned(),
            expected: "Bucket".to_owned(),
            actual: "Other".to_owned(),
        }
    );

    let error = program
        .insert_row(
            "BucketIn",
            [Value::typed(
                "Bucket",
                Value::typed("Other", Value::Number(19)),
            )],
        )
        .unwrap_err();
    assert_eq!(
        error,
        SouffleError::TypeMismatch {
            relation: "BucketIn".to_owned(),
            column: "value".to_owned(),
            expected: "Bucket".to_owned(),
            actual: "Other".to_owned(),
        }
    );
}

#[test]
fn relation_handles_preserve_schema_identity_and_capabilities() {
    let schema = sample_schema();
    let handles = schema.handles().collect::<Vec<_>>();

    assert_eq!(handles.len(), 2);
    assert!(handles.iter().any(|handle| {
        handle.name() == "Input"
            && handle.id() == RelationId::new(0)
            && handle.kind() == RelationKind::Input
            && handle.is_loadable()
            && !handle.is_printable()
    }));
    assert!(handles.iter().any(|handle| {
        handle.name() == "Output"
            && handle.id() == RelationId::new(1)
            && handle.kind() == RelationKind::Output
            && !handle.is_loadable()
            && handle.is_printable()
    }));
}

#[test]
fn program_uses_relation_handles_for_dynamic_operations() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let input = program.relation_handle("Input").unwrap();
    let output = program.relation_handle("Output").unwrap();

    program
        .insert_row_by_handle(&input, [Value::Number(1), Value::Symbol("entry".into())])
        .unwrap();
    program
        .replace_relation_rows(
            output.name(),
            [Row::new(vec![
                Value::Number(1),
                Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
            ])],
        )
        .unwrap();

    let rows = program.read_relation_by_handle(&output).unwrap();
    assert_eq!(rows.schema().id(), output.id());
    assert_eq!(rows.rows().len(), 1);
}

#[test]
fn stale_relation_handle_reports_typed_mismatch() {
    let program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let stale = RelationHandle::new(
        RelationId::new(99),
        "Output",
        RelationKind::Output,
        false,
        true,
    );

    let error = program.relation_schema_by_handle(&stale).unwrap_err();

    assert_eq!(
        error,
        SouffleError::RelationHandleMismatch {
            relation: "Output".to_owned(),
            expected: RelationId::new(99),
            actual: RelationId::new(1),
        }
    );
}

#[cfg(unix)]
#[test]
fn process_backend_runs_fact_file_exchange() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
set -eu
facts=""
output=""
threads=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -F) facts="$2"; shift 2 ;;
    -D) output="$2"; shift 2 ;;
    -j) threads="$2"; shift 2 ;;
    *) echo "unexpected arg: $1" >&2; exit 2 ;;
  esac
done
test -n "$facts"
test -n "$output"
test -n "$threads"
cp "$facts/Input.facts" "$output/Output.csv"
test -f "$facts/Trigger.facts"
printf '()\n' > "$output/Fired.csv"
printf "%s" "$threads" > "$output/threads.txt"
"#,
    );

    let schema = RelationBundle::from_iter([
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new("label", TypeRef::Symbol),
            ],
        ),
        RelationSchema::input(RelationId::new(1), "Trigger", []),
        RelationSchema::output(
            RelationId::new(2),
            "Output",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new("label", TypeRef::Symbol),
            ],
        ),
        RelationSchema::output(RelationId::new(3), "Fired", []),
    ]);
    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    program
        .insert_row("Input", [Value::Number(7), Value::Symbol("entry".into())])
        .unwrap();
    program.insert_row("Trigger", Row::new(Vec::new())).unwrap();
    program
        .run_with_options(RunOptions::new(NonZeroUsize::new(3).unwrap()))
        .unwrap();

    assert_eq!(program.backend(), Backend::Process);
    assert_eq!(
        fs::read_to_string(work_dir.join("facts/Input.facts")).unwrap(),
        "7\tentry\n"
    );
    assert_eq!(
        fs::read_to_string(work_dir.join("facts/Trigger.facts")).unwrap(),
        "\n"
    );
    assert_eq!(
        fs::read_to_string(work_dir.join("output/threads.txt")).unwrap(),
        "3"
    );
    assert_eq!(
        program.read_relation("Output").unwrap().rows(),
        &[Row::new(vec![
            Value::Number(7),
            Value::Symbol("entry".to_owned())
        ])]
    );
    assert_eq!(
        program.read_relation("Fired").unwrap().rows(),
        &[Row::new(Vec::new())]
    );
}

#[cfg(unix)]
#[test]
fn process_backend_refuses_unmanaged_facts_directory() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
exit 0
"#,
    );
    let schema = RelationBundle::from_iter([
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [AttributeSchema::new("id", TypeRef::Number)],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [AttributeSchema::new("id", TypeRef::Number)],
        ),
    ]);
    let work_dir = tempdir.path().join("work");
    let facts_dir = work_dir.join("facts");
    fs::create_dir_all(&facts_dir).unwrap();
    fs::write(facts_dir.join("keep.txt"), "user data").unwrap();

    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();
    program.insert_row("Input", [Value::Number(7)]).unwrap();

    let error = program.run().unwrap_err();
    match error {
        SouffleError::FileIo {
            operation,
            path,
            message,
        } => {
            assert_eq!(operation, "prepare directory");
            assert_eq!(path, facts_dir.display().to_string());
            assert!(message.contains("unmanaged process exchange directory"));
        }
        error => panic!("expected unmanaged facts directory failure, got {error:?}"),
    }
    assert_eq!(
        fs::read_to_string(facts_dir.join("keep.txt")).unwrap(),
        "user data"
    );
}

#[cfg(unix)]
#[test]
fn process_backend_refuses_unmanaged_output_directory() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
exit 0
"#,
    );
    let schema = RelationBundle::from_iter([
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [AttributeSchema::new("id", TypeRef::Number)],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [AttributeSchema::new("id", TypeRef::Number)],
        ),
    ]);
    let work_dir = tempdir.path().join("work");
    let output_dir = work_dir.join("output");
    fs::create_dir_all(&output_dir).unwrap();
    fs::write(output_dir.join("keep.txt"), "user data").unwrap();

    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();
    program.insert_row("Input", [Value::Number(7)]).unwrap();

    let error = program.run().unwrap_err();
    match error {
        SouffleError::FileIo {
            operation,
            path,
            message,
        } => {
            assert_eq!(operation, "prepare directory");
            assert_eq!(path, output_dir.display().to_string());
            assert!(message.contains("unmanaged process exchange directory"));
        }
        error => panic!("expected unmanaged output directory failure, got {error:?}"),
    }
    assert_eq!(
        fs::read_to_string(output_dir.join("keep.txt")).unwrap(),
        "user data"
    );
}

#[cfg(unix)]
#[test]
fn process_backend_rejects_fact_delimiter_symbols_on_insert() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
exit 0
"#,
    );

    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(scalar_process_schema())
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    let error = program
        .insert_row(
            "Input",
            [Value::Number(7), Value::Symbol("line\nfeed".to_owned())],
        )
        .unwrap_err();

    match error {
        SouffleError::EncodeFailed { artifact, message } => {
            assert_eq!(artifact, "Input.facts");
            assert!(message.contains("delimiter"));
        }
        error => panic!("expected process fact encode failure, got {error:?}"),
    }
}

#[cfg(unix)]
#[test]
fn process_backend_preserves_declared_identity_for_subtype_and_union_io() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
set -eu
facts=""
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -F) facts="$2"; shift 2 ;;
    -D) output="$2"; shift 2 ;;
    -j) shift 2 ;;
    *) echo "unexpected arg: $1" >&2; exit 2 ;;
  esac
done
cat "$facts/InputSmall.facts" > "$output/SmallOut.csv"
cat "$facts/InputSmall.facts" > "$output/BucketOut.csv"
"#,
    );

    let small = TypeRef::Subtype {
        name: "Small".to_owned(),
        base: Box::new(TypeRef::Number),
    };
    let large = TypeRef::Subtype {
        name: "Large".to_owned(),
        base: Box::new(TypeRef::Number),
    };
    let bucket = TypeRef::Union {
        name: "Bucket".to_owned(),
        variants: vec![small.clone(), large],
    };
    let schema = RelationBundle::from_iter([
        RelationSchema::input(
            RelationId::new(0),
            "InputSmall",
            [AttributeSchema::new("value", small.clone())],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "SmallOut",
            [AttributeSchema::new("value", small)],
        ),
        RelationSchema::output(
            RelationId::new(2),
            "BucketOut",
            [AttributeSchema::new("value", bucket)],
        ),
    ]);
    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    let error = program
        .insert_row("InputSmall", [Value::typed("Large", Value::Number(7))])
        .unwrap_err();
    assert_eq!(
        error,
        SouffleError::TypeMismatch {
            relation: "InputSmall".to_owned(),
            column: "value".to_owned(),
            expected: "Small".to_owned(),
            actual: "Large".to_owned(),
        }
    );

    program
        .insert_row("InputSmall", [Value::typed("Small", Value::Number(7))])
        .unwrap();
    program.run().unwrap();

    assert_eq!(
        fs::read_to_string(work_dir.join("facts/InputSmall.facts")).unwrap(),
        "7\n"
    );
    assert_eq!(
        program.read_relation("SmallOut").unwrap().rows(),
        &[Row::new([Value::typed("Small", Value::Number(7))])]
    );
    assert_eq!(
        program.read_relation("BucketOut").unwrap().rows(),
        &[Row::new([Value::typed(
            "Bucket",
            Value::typed("Small", Value::Number(7))
        )])]
    );
}

#[cfg(unix)]
#[test]
fn process_backend_decodes_single_symbol_output_with_tab() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
set -eu
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -F) shift 2 ;;
    -D) output="$2"; shift 2 ;;
    -j) shift 2 ;;
    *) echo "unexpected arg: $1" >&2; exit 2 ;;
  esac
done
printf 'plain	tab\n' > "$output/Output.csv"
"#,
    );

    let schema = [RelationSchema::output(
        RelationId::new(0),
        "Output",
        [AttributeSchema::new("label", TypeRef::Symbol)],
    )]
    .into_iter()
    .collect();
    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    program.run().unwrap();

    assert_eq!(
        program.read_relation("Output").unwrap().rows(),
        &[Row::new([Value::Symbol("plain\ttab".to_owned())])]
    );
}

#[cfg(unix)]
#[test]
fn process_backend_reports_nonzero_process_exit() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
echo "partial stdout"
echo "fatal stderr" >&2
exit 17
"#,
    );

    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(scalar_process_schema())
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    let error = program.run().unwrap_err();

    match error {
        SouffleError::RunFailed { program, message } => {
            assert_eq!(program, "analysis");
            assert!(message.contains("status"));
            assert!(message.contains("17"));
            assert!(message.contains("partial stdout"));
            assert!(message.contains("fatal stderr"));
        }
        error => panic!("expected process run failure, got {error:?}"),
    }
}

#[cfg(unix)]
#[test]
fn process_backend_kills_timed_out_process() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
echo "starting"
exec sleep 5
"#,
    );

    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(scalar_process_schema())
        .process_config(
            ProcessConfig::new(&runner, &work_dir).with_timeout(Duration::from_millis(50)),
        )
        .build_process()
        .unwrap();

    assert_eq!(
        program.process_config().timeout(),
        Some(Duration::from_millis(50))
    );

    let error = program.run().unwrap_err();

    match error {
        SouffleError::RunFailed { program, message } => {
            assert_eq!(program, "analysis");
            assert!(message.contains("timed out after"));
            assert!(message.contains("starting"));
        }
        error => panic!("expected process timeout failure, got {error:?}"),
    }
}

#[cfg(unix)]
#[test]
fn process_backend_runs_composite_fact_file_exchange() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
set -eu
facts=""
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -F) facts="$2"; shift 2 ;;
    -D) output="$2"; shift 2 ;;
    -j) shift 2 ;;
    *) echo "unexpected arg: $1" >&2; exit 2 ;;
  esac
done
cat "$facts/Input.facts" > "$output/Output.csv"
printf '[9, empty]\tnil\t$Empty\n' >> "$output/Output.csv"
"#,
    );

    let pair = TypeRef::Record(vec![TypeRef::Number, TypeRef::Symbol]);
    let list = TypeRef::List(Box::new(TypeRef::Number));
    let choice = TypeRef::adt(
        "Choice",
        [
            ("Some".to_owned(), vec![pair.clone(), list.clone()]),
            ("Empty".to_owned(), Vec::new()),
        ],
    );
    let schema = RelationBundle::from_iter([
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("payload", pair.clone()),
                AttributeSchema::new("numbers", list.clone()),
                AttributeSchema::new("choice", choice.clone()),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [
                AttributeSchema::new("payload", pair),
                AttributeSchema::new("numbers", list),
                AttributeSchema::new("choice", choice),
            ],
        ),
    ]);
    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    let payload = Value::Record(vec![Value::Number(7), Value::Symbol("entry".to_owned())]);
    let numbers = Value::List(vec![Value::Number(1), Value::Number(2)]);
    let choice = Value::Adt {
        variant: "Some".to_owned(),
        fields: vec![payload.clone(), numbers.clone()],
    };
    program
        .insert_row("Input", [payload.clone(), numbers.clone(), choice.clone()])
        .unwrap();
    program.run().unwrap();

    assert_eq!(
        fs::read_to_string(work_dir.join("facts/Input.facts")).unwrap(),
        "[7, \"entry\"]\t[1, [2, nil]]\t$Some([7, \"entry\"], [1, [2, nil]])\n"
    );

    let expected_first = Row::new(vec![payload, numbers, choice]);
    let expected_second = Row::new(vec![
        Value::Record(vec![Value::Number(9), Value::Symbol("empty".to_owned())]),
        Value::List(Vec::new()),
        Value::Adt {
            variant: "Empty".to_owned(),
            fields: Vec::new(),
        },
    ]);
    let mut rows = program.iter_relation("Output").unwrap();
    assert_eq!(rows.next_chunk(1).unwrap(), vec![expected_first.clone()]);
    assert_eq!(rows.next_chunk(8).unwrap(), vec![expected_second.clone()]);
    assert!(rows.next_chunk(8).unwrap().is_empty());
    assert_eq!(
        program.read_relation("Output").unwrap().rows(),
        &[expected_first, expected_second]
    );
}

#[cfg(unix)]
#[test]
fn process_backend_decodes_multiline_composite_symbol_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
set -eu
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -F) shift 2 ;;
    -D) output="$2"; shift 2 ;;
    -j) shift 2 ;;
    *) echo "unexpected arg: $1" >&2; exit 2 ;;
  esac
done
printf '[line\nfeed]\n' > "$output/Output.csv"
"#,
    );

    let payload = TypeRef::Record(vec![TypeRef::Symbol]);
    let schema = [RelationSchema::output(
        RelationId::new(0),
        "Output",
        [AttributeSchema::new("payload", payload)],
    )]
    .into_iter()
    .collect();
    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    program.run().unwrap();

    assert_eq!(
        program.read_relation("Output").unwrap().rows(),
        &[Row::new([Value::Record(vec![Value::Symbol(
            "line\nfeed".to_owned()
        )])])]
    );
}

#[cfg(unix)]
#[test]
fn process_backend_ignores_composite_delimiters_inside_quoted_symbol_output() {
    let tempdir = tempfile::tempdir().unwrap();
    let runner = tempdir.path().join("runner.sh");
    write_executable_script(
        &runner,
        r#"#!/bin/sh
set -eu
output=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -F) shift 2 ;;
    -D) output="$2"; shift 2 ;;
    -j) shift 2 ;;
    *) echo "unexpected arg: $1" >&2; exit 2 ;;
  esac
done
printf '["open [ bracket and quote \\""]\n' > "$output/Output.csv"
"#,
    );

    let payload = TypeRef::Record(vec![TypeRef::Symbol]);
    let schema = [RelationSchema::output(
        RelationId::new(0),
        "Output",
        [AttributeSchema::new("payload", payload)],
    )]
    .into_iter()
    .collect();
    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(schema)
        .process_config(ProcessConfig::new(&runner, &work_dir))
        .build_process()
        .unwrap();

    program.run().unwrap();

    assert_eq!(
        program.read_relation("Output").unwrap().rows(),
        &[Row::new([Value::Record(vec![Value::Symbol(
            "open [ bracket and quote \"".to_owned()
        )])])]
    );
}

#[cfg(unix)]
fn write_executable_script(path: &std::path::Path, contents: &str) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let tmp_path = path.with_extension("sh.tmp");
    let mut file = fs::File::create(&tmp_path).unwrap();
    file.write_all(contents.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);

    let mut permissions = fs::metadata(&tmp_path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&tmp_path, permissions).unwrap();
    fs::rename(&tmp_path, path).unwrap();
}

#[test]
fn ffi_status_mapping_preserves_typed_error_context() {
    let error = ffi_error(SouffleRsStatus::Error, b"insert failed");

    let result = check_status("souffle_rs_program_insert_row", 1, &error);

    assert_eq!(
        result.unwrap_err(),
        SouffleError::Abi(crate::AbiError::CallFailed {
            function: "souffle_rs_program_insert_row".to_owned(),
            status: "error".to_owned(),
            message: "insert failed".to_owned(),
        })
    );
}

#[test]
fn ffi_status_mapping_preserves_cxx_exception() {
    let error = ffi_error(SouffleRsStatus::Exception, b"bad tuple");

    let result = check_status("souffle_rs_program_run", 2, &error);

    assert_eq!(
        result.unwrap_err(),
        SouffleError::CxxException {
            message: "bad tuple".to_owned(),
        }
    );
}

#[test]
fn ffi_status_mapping_rejects_invalid_utf8_messages() {
    let error = ffi_error(SouffleRsStatus::Error, &[0xff]);

    let result = check_status("souffle_rs_program_run", 1, &error);

    match result.unwrap_err() {
        SouffleError::Abi(crate::AbiError::InvalidString { argument, .. }) => {
            assert_eq!(argument, "SouffleRsError.message");
        }
        error => panic!("expected invalid string error, got {error:?}"),
    }
}

#[test]
fn ffi_status_mapping_handles_unknown_codes_and_abi_versions() {
    let error = crate::ffi::empty_error();

    assert_eq!(
        check_status("unknown", 99, &error).unwrap_err(),
        SouffleError::Abi(crate::AbiError::UnknownErrorCode { code: 99 })
    );
    assert_eq!(
        check_abi_version(0).unwrap_err(),
        SouffleError::Abi(crate::AbiError::VersionMismatch {
            expected: souffle_rs_sys::SOUFFLE_RS_ABI_VERSION,
            actual: 0,
        })
    );
    check_abi_version(souffle_rs_sys::SOUFFLE_RS_ABI_VERSION).unwrap();
}

#[test]
fn embedded_program_rejects_interior_nul_names_before_ffi() {
    let error = program_name_cstring("bad\0name").unwrap_err();

    match error {
        SouffleError::Abi(crate::AbiError::InvalidString { argument, .. }) => {
            assert_eq!(argument, "program_name");
        }
        error => panic!("expected invalid program name string, got {error:?}"),
    }
}

#[test]
fn embedded_scalar_input_rows_encode_c_abi_values() {
    let schema = RelationSchema::input(
        RelationId::new(0),
        "Input",
        [
            AttributeSchema::new("id", TypeRef::Number),
            AttributeSchema::new("count", TypeRef::Unsigned),
            AttributeSchema::new("score", TypeRef::Float),
            AttributeSchema::new("label", TypeRef::Symbol),
            AttributeSchema::new("marker", TypeRef::Nullary),
        ],
    );
    let row = Row::new(vec![
        Value::Number(-7),
        Value::Unsigned(9),
        Value::Float(-0.0),
        Value::Symbol("entry".to_owned()),
        Value::Nullary,
    ]);

    let encoded = encode_input_row(&schema, &row).unwrap();
    let abi_row = encoded.as_ffi();

    assert_eq!(abi_row.len, 5);
    assert_eq!(encoded.values()[0].kind, SouffleRsValueKind::Number);
    assert_eq!(encoded.values()[1].kind, SouffleRsValueKind::Unsigned);
    assert_eq!(encoded.values()[2].kind, SouffleRsValueKind::Float);
    assert_eq!(encoded.values()[3].kind, SouffleRsValueKind::Symbol);
    assert_eq!(encoded.values()[4].kind, SouffleRsValueKind::Nullary);
    unsafe {
        assert_eq!(encoded.values()[0].as_.number, -7);
        assert_eq!(encoded.values()[1].as_.unsigned_value, 9);
        assert_eq!(
            encoded.values()[2].as_.float_value.to_bits(),
            (-0.0f64).to_bits()
        );
        assert_eq!(encoded.values()[3].as_.symbol.len, 5);
    }
}

#[test]
fn embedded_typed_subtype_input_rows_encode_runtime_abi_values() {
    let schema = RelationSchema::input(
        RelationId::new(0),
        "Input",
        [AttributeSchema::new(
            "value",
            TypeRef::Subtype {
                name: "Small".to_owned(),
                base: Box::new(TypeRef::Number),
            },
        )],
    );
    let row = Row::new([Value::typed("Small", Value::Number(7))]);

    let encoded = encode_input_row(&schema, &row).unwrap();

    assert_eq!(encoded.values()[0].kind, SouffleRsValueKind::Number);
    unsafe {
        assert_eq!(encoded.values()[0].as_.number, 7);
    }

    let error = match encode_input_row(
        &schema,
        &Row::new([Value::typed("Large", Value::Number(7))]),
    ) {
        Ok(_) => panic!("expected typed subtype mismatch"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        SouffleError::TypeMismatch {
            relation: "Input".to_owned(),
            column: "value".to_owned(),
            expected: "Small".to_owned(),
            actual: "Large".to_owned(),
        }
    );
}

#[test]
fn embedded_union_input_rows_encode_selected_declared_type() {
    let small = TypeRef::Subtype {
        name: "Small".to_owned(),
        base: Box::new(TypeRef::Number),
    };
    let large = TypeRef::Subtype {
        name: "Large".to_owned(),
        base: Box::new(TypeRef::Number),
    };
    let schema = RelationSchema::input(
        RelationId::new(0),
        "Input",
        [AttributeSchema::new(
            "value",
            TypeRef::Union {
                name: "Bucket".to_owned(),
                variants: vec![small, large],
            },
        )],
    );

    let encoded = encode_input_row(
        &schema,
        &Row::new([Value::typed(
            "Bucket",
            Value::typed("Large", Value::Number(7)),
        )]),
    )
    .unwrap();

    assert_eq!(encoded.values()[0].kind, SouffleRsValueKind::Number);
    assert_eq!(
        borrowed_abi_string(encoded.values()[0].declared_type),
        "Large"
    );
    unsafe {
        assert_eq!(encoded.values()[0].as_.number, 7);
    }
}

#[test]
fn embedded_composite_input_rows_encode_borrowed_c_abi_arena() {
    let schema = RelationSchema::input(
        RelationId::new(0),
        "Input",
        [
            AttributeSchema::new(
                "payload",
                TypeRef::Record(vec![TypeRef::Unsigned, TypeRef::Symbol]),
            ),
            AttributeSchema::new("numbers", TypeRef::List(Box::new(TypeRef::Number))),
            AttributeSchema::new(
                "choice",
                TypeRef::adt("Choice", [("Some".to_owned(), vec![TypeRef::Symbol])]),
            ),
        ],
    );
    let row = Row::new(vec![
        Value::Record(vec![Value::Unsigned(9), Value::Symbol("inner".to_owned())]),
        Value::List(vec![Value::Number(1), Value::Number(2)]),
        Value::Adt {
            variant: "Some".to_owned(),
            fields: vec![Value::Symbol("tag".to_owned())],
        },
    ]);

    let encoded = encode_input_row(&schema, &row).unwrap();
    let abi_row = encoded.as_ffi();

    assert_eq!(abi_row.len, 3);
    assert_eq!(abi_row.composite_count, 3);
    assert_eq!(encoded.values()[0].kind, SouffleRsValueKind::Record);
    assert_eq!(encoded.values()[1].kind, SouffleRsValueKind::List);
    assert_eq!(encoded.values()[2].kind, SouffleRsValueKind::Adt);
    unsafe {
        assert_eq!(encoded.values()[0].as_.composite.index, 0);
        assert_eq!(encoded.values()[1].as_.composite.index, 1);
        assert_eq!(encoded.values()[2].as_.composite.index, 2);
    }

    let composites = encoded.composites();
    assert_eq!(composites[0].kind, SouffleRsValueKind::Record);
    assert_eq!(composites[0].len, 2);
    assert_eq!(composites[1].kind, SouffleRsValueKind::List);
    assert_eq!(composites[1].len, 2);
    assert_eq!(composites[2].kind, SouffleRsValueKind::Adt);
    assert_eq!(composites[2].len, 1);
    assert_eq!(composites[2].variant.len, 4);

    let record_values = unsafe { std::slice::from_raw_parts(composites[0].values, 2) };
    assert_eq!(record_values[0].kind, SouffleRsValueKind::Unsigned);
    assert_eq!(record_values[1].kind, SouffleRsValueKind::Symbol);
    let list_values = unsafe { std::slice::from_raw_parts(composites[1].values, 2) };
    assert_eq!(list_values[0].kind, SouffleRsValueKind::Number);
    assert_eq!(list_values[1].kind, SouffleRsValueKind::Number);
    let adt_values = unsafe { std::slice::from_raw_parts(composites[2].values, 1) };
    assert_eq!(adt_values[0].kind, SouffleRsValueKind::Symbol);
}

fn borrowed_abi_string(value: SouffleRsString) -> String {
    if value.len == 0 {
        return String::new();
    }
    assert!(!value.data.is_null());
    let bytes = unsafe { std::slice::from_raw_parts(value.data.cast::<u8>(), value.len) };
    std::str::from_utf8(bytes).unwrap().to_owned()
}

#[test]
fn embedded_scalar_output_decode_preserves_float_bits_and_symbols() {
    let raw_float = SouffleRsValue {
        kind: SouffleRsValueKind::Float,
        declared_type: SouffleRsString::null(),
        as_: SouffleRsValueData {
            float_value: f64::from_bits(0x8000_0000_0000_0000),
        },
    };
    let symbol = b"entry";
    let raw_symbol = SouffleRsValue {
        kind: SouffleRsValueKind::Symbol,
        declared_type: SouffleRsString::null(),
        as_: SouffleRsValueData {
            symbol: SouffleRsString {
                data: symbol.as_ptr().cast(),
                len: symbol.len(),
            },
        },
    };

    assert_float_bits(
        decode_scalar_value("Output", "score", &TypeRef::Float, &raw_float).unwrap(),
        0x8000_0000_0000_0000,
    );
    assert_eq!(
        decode_scalar_value("Output", "label", &TypeRef::Symbol, &raw_symbol).unwrap(),
        Value::Symbol("entry".to_owned())
    );
}

#[test]
fn embedded_scalar_output_decode_preserves_subtype_wrapper() {
    let raw_value = SouffleRsValue {
        kind: SouffleRsValueKind::Number,
        declared_type: SouffleRsString::null(),
        as_: SouffleRsValueData { number: 9 },
    };
    let small = TypeRef::Subtype {
        name: "Small".to_owned(),
        base: Box::new(TypeRef::Number),
    };

    assert_eq!(
        decode_scalar_value("Output", "value", &small, &raw_value).unwrap(),
        Value::typed("Small", Value::Number(9))
    );
}

#[test]
fn embedded_scalar_output_decode_reports_kind_mismatch() {
    let raw_value = SouffleRsValue {
        kind: SouffleRsValueKind::Unsigned,
        declared_type: SouffleRsString::null(),
        as_: SouffleRsValueData { unsigned_value: 1 },
    };

    let error = decode_scalar_value("Output", "id", &TypeRef::Number, &raw_value).unwrap_err();

    assert_eq!(
        error,
        SouffleError::DecodeFailed {
            relation: "Output".to_owned(),
            column: "id".to_owned(),
            message: "expected `number` but ABI value kind was `unsigned`".to_owned(),
        }
    );
}

#[test]
fn type_mismatch_preserves_relation_and_column() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();

    let error = program
        .insert_row("Input", [Value::Number(1), Value::Unsigned(2)])
        .unwrap_err();

    assert_eq!(
        error,
        SouffleError::TypeMismatch {
            relation: "Input".to_owned(),
            column: "label".to_owned(),
            expected: "symbol".to_owned(),
            actual: "unsigned".to_owned(),
        }
    );
}

#[test]
fn arity_mismatch_preserves_relation_context() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();

    let error = program.insert_row("Input", [Value::Number(1)]).unwrap_err();

    assert_eq!(
        error,
        SouffleError::ArityMismatch {
            relation: "Input".to_owned(),
            expected: 2,
            actual: 1,
        }
    );
}

#[test]
fn output_iteration_requires_printable_relation() {
    let program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();

    let error = program.iter_relation("Input").unwrap_err();

    assert_eq!(
        error,
        SouffleError::RelationNotOutput {
            relation: "Input".to_owned(),
        }
    );
}

#[test]
fn output_iteration_streams_schema_checked_rows() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();

    program
        .replace_relation_rows(
            "Output",
            [Row::new(vec![
                Value::Number(1),
                Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
            ])],
        )
        .unwrap();

    let mut rows = program.iter_relation("Output").unwrap();
    assert_eq!(rows.schema().name(), "Output");
    assert_eq!(
        rows.next_row().unwrap(),
        Some(Row::new(vec![
            Value::Number(1),
            Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
        ]))
    );
    assert_eq!(rows.next_row().unwrap(), None);
}

#[test]
fn output_iteration_streams_bounded_chunks() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let expected_rows = vec![
        Row::new(vec![
            Value::Number(1),
            Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
        ]),
        Row::new(vec![
            Value::Number(2),
            Value::Record(vec![Value::Unsigned(8), Value::Float(2.5)]),
        ]),
        Row::new(vec![
            Value::Number(3),
            Value::Record(vec![Value::Unsigned(9), Value::Float(3.5)]),
        ]),
    ];

    program
        .replace_relation_rows("Output", expected_rows.clone())
        .unwrap();

    let mut rows = program.iter_relation("Output").unwrap();

    assert_eq!(rows.next_chunk(0).unwrap(), Vec::<Row>::new());
    assert_eq!(rows.next_chunk(2).unwrap(), expected_rows[..2].to_vec());
    assert_eq!(rows.next_chunk(2).unwrap(), expected_rows[2..].to_vec());
    assert_eq!(rows.next_chunk(2).unwrap(), Vec::<Row>::new());
}

#[test]
fn adt_variant_errors_are_distinct_from_type_mismatch() {
    let adt = TypeRef::adt("MaybeNumber", [("Some".to_owned(), vec![TypeRef::Number])]);
    let schema = [RelationSchema::input(
        RelationId::new(0),
        "Input",
        [AttributeSchema::new("value", adt)],
    )]
    .into_iter()
    .collect();
    let mut program = InMemoryProgram::builder("analysis")
        .schema(schema)
        .build_memory();

    let error = program
        .insert_row(
            "Input",
            [Value::Adt {
                variant: "None".to_owned(),
                fields: Vec::new(),
            }],
        )
        .unwrap_err();

    assert_eq!(
        error,
        SouffleError::AdtVariantMismatch {
            relation: "Input".to_owned(),
            column: "value".to_owned(),
            variant: "None".to_owned(),
        }
    );
}

#[test]
fn recursive_adt_references_validate_nested_values() {
    let expr = TypeRef::adt(
        "Expr",
        [
            ("Const".to_owned(), vec![TypeRef::Number]),
            (
                "Add".to_owned(),
                vec![
                    TypeRef::Reference {
                        name: "Expr".to_owned(),
                        runtime: ValueKind::Adt,
                    },
                    TypeRef::Reference {
                        name: "Expr".to_owned(),
                        runtime: ValueKind::Adt,
                    },
                ],
            ),
            ("Name".to_owned(), vec![TypeRef::Symbol]),
        ],
    );
    let schema = [RelationSchema::input(
        RelationId::new(0),
        "Input",
        [AttributeSchema::new("expr", expr)],
    )]
    .into_iter()
    .collect();
    let mut program = InMemoryProgram::builder("analysis")
        .schema(schema)
        .build_memory();

    program
        .insert_row(
            "Input",
            [Value::Adt {
                variant: "Add".to_owned(),
                fields: vec![
                    Value::Adt {
                        variant: "Const".to_owned(),
                        fields: vec![Value::Number(1)],
                    },
                    Value::Adt {
                        variant: "Name".to_owned(),
                        fields: vec![Value::Symbol("entry".to_owned())],
                    },
                ],
            }],
        )
        .unwrap();

    let error = program
        .insert_row(
            "Input",
            [Value::Adt {
                variant: "Add".to_owned(),
                fields: vec![
                    Value::Adt {
                        variant: "Const".to_owned(),
                        fields: vec![Value::Number(1)],
                    },
                    Value::Adt {
                        variant: "Missing".to_owned(),
                        fields: Vec::new(),
                    },
                ],
            }],
        )
        .unwrap_err();

    assert_eq!(
        error,
        SouffleError::AdtVariantMismatch {
            relation: "Input".to_owned(),
            column: "expr".to_owned(),
            variant: "Missing".to_owned(),
        }
    );
}

#[test]
fn schema_validation_rejects_adt_without_variant_order() {
    let adt = TypeRef::Adt {
        name: "Choice".to_owned(),
        variants: [
            ("Some".to_owned(), vec![TypeRef::Number]),
            ("None".to_owned(), Vec::new()),
        ]
        .into_iter()
        .collect(),
        variant_order: Vec::new(),
        is_enum: false,
    };
    let schema = [RelationSchema::input(
        RelationId::new(0),
        "Input",
        [AttributeSchema::new("choice", adt)],
    )]
    .into_iter()
    .collect();

    let error = InMemoryProgram::builder("analysis")
        .schema(schema)
        .try_build_memory()
        .unwrap_err();

    match error {
        SouffleError::SchemaValidation {
            relation,
            path,
            message,
        } => {
            assert_eq!(relation, "Input");
            assert_eq!(path, "choice");
            assert!(message.contains("variant_order"));
        }
        error => panic!("expected schema validation error, got {error:?}"),
    }
}

#[test]
fn run_options_keep_explicit_thread_count() {
    let mut program = InMemoryProgram::builder("analysis")
        .threads(NonZeroUsize::new(4).unwrap())
        .schema(sample_schema())
        .build_memory();
    let options = RunOptions::new(NonZeroUsize::new(8).unwrap());

    program.run_with_options(options.clone()).unwrap();

    assert_eq!(program.last_run_options(), Some(&options));
}

#[test]
fn run_uses_configured_default_souffle_threads() {
    let mut program = InMemoryProgram::builder("analysis")
        .cpu_budget(CpuBudget::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroUsize::new(4).unwrap(),
        ))
        .schema(sample_schema())
        .build_memory();

    program.run().unwrap();

    assert_eq!(
        program.last_run_options(),
        Some(&RunOptions::new(NonZeroUsize::new(4).unwrap()))
    );
}

#[test]
fn cpu_budget_reports_typed_oversubscription() {
    let budget = CpuBudget::new(NonZeroUsize::new(3).unwrap(), NonZeroUsize::new(4).unwrap());

    assert_eq!(budget.max_concurrent_threads(), 12);
    assert_eq!(
        budget.validate_against_available_parallelism(NonZeroUsize::new(8).unwrap()),
        Err(SouffleError::ThreadOversubscription {
            rust_workers: 3,
            souffle_threads: 4,
            requested_threads: 12,
            available_threads: 8,
        })
    );

    let config = ProgramConfig::new("analysis").with_cpu_budget(budget);
    assert_eq!(
        config.validate_cpu_budget(NonZeroUsize::new(12).unwrap()),
        Ok(())
    );
}

#[test]
fn file_relation_store_exports_manifest_schema_and_rows() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let exact_nan = f64::from_bits(0x7ff8_0000_0000_0123);
    program
        .replace_relation_rows(
            "Output",
            [Row::new(vec![
                Value::Number(1),
                Value::Record(vec![Value::Unsigned(7), Value::Float(exact_nan)]),
            ])],
        )
        .unwrap();

    let tempdir = tempfile::tempdir().unwrap();
    let store = FileRelationStore::new(tempdir.path());
    let manifest = store.export_outputs(&program, ["Output"]).unwrap();

    assert_eq!(manifest.format_version, 1);
    assert_eq!(
        manifest.schema_path,
        std::path::PathBuf::from("schema.json")
    );
    assert_eq!(manifest.relations.len(), 1);
    assert_eq!(manifest.relations[0].relation, "Output");
    assert_eq!(manifest.relations[0].row_count, 1);
    assert!(tempdir.path().join("manifest.json").exists());
    assert!(tempdir.path().join("schema.json").exists());
    assert!(
        tempdir
            .path()
            .join(&manifest.relations[0].rows_path)
            .exists()
    );

    let schema_json = fs::read_to_string(tempdir.path().join("schema.json")).unwrap();
    assert!(schema_json.contains("Output"));

    let loaded = store.load_outputs().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].schema().name(), "Output");
    assert_eq!(loaded[0].rows().len(), 1);
    assert_nested_float_bits(&loaded[0].rows()[0], 0x7ff8_0000_0000_0123);
}

#[test]
fn file_relation_store_exports_outputs_from_iterator_without_read_relation() {
    let output_rows = vec![
        Row::new(vec![
            Value::Number(6),
            Value::Record(vec![Value::Unsigned(12), Value::Float(-0.0)]),
        ]),
        Row::new(vec![
            Value::Number(7),
            Value::Record(vec![Value::Unsigned(13), Value::Float(4.5)]),
        ]),
    ];
    let program = IteratorOnlyProgram::new(output_rows.clone());
    let tempdir = tempfile::tempdir().unwrap();
    let store = FileRelationStore::new(tempdir.path());

    let manifest = store.export_outputs(&program, ["Output"]).unwrap();

    assert_eq!(manifest.relations.len(), 1);
    assert_eq!(manifest.relations[0].row_count, output_rows.len());
    assert_eq!(
        store.load_outputs().unwrap()[0].rows(),
        output_rows.as_slice()
    );
}

#[test]
fn file_program_persists_dynamic_rows_and_iterates_outputs() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = FileRelationStore::new(tempdir.path().join("runtime"));
    let output_rows = vec![
        Row::new(vec![
            Value::Number(6),
            Value::Record(vec![Value::Unsigned(12), Value::Float(-0.0)]),
        ]),
        Row::new(vec![
            Value::Number(7),
            Value::Record(vec![Value::Unsigned(13), Value::Float(4.5)]),
        ]),
    ];

    let mut program = FileProgram::builder("analysis")
        .schema(sample_schema())
        .file_store(store.clone())
        .build_file()
        .unwrap();
    program
        .insert_row(
            "Input",
            [Value::Number(10), Value::Symbol("persist".into())],
        )
        .unwrap();
    program
        .replace_relation_rows("Output", output_rows.clone())
        .unwrap();
    let run_options = RunOptions::new(NonZeroUsize::new(5).unwrap());
    program.run_with_options(run_options.clone()).unwrap();

    assert_eq!(program.backend(), Backend::File);
    assert_eq!(program.store().root(), store.root());
    assert_eq!(program.last_run_options(), Some(&run_options));
    assert!(store.root().join("manifest.json").exists());
    assert!(store.root().join("schema.json").exists());

    let manifest = store.load_manifest().unwrap();
    assert_eq!(manifest.relations.len(), 2);
    assert_eq!(
        manifest
            .relations
            .iter()
            .find(|artifact| artifact.relation == "Input")
            .unwrap()
            .row_count,
        1
    );
    assert_eq!(
        manifest
            .relations
            .iter()
            .find(|artifact| artifact.relation == "Output")
            .unwrap()
            .row_count,
        2
    );

    let mut rows = program.iter_relation("Output").unwrap();
    assert_eq!(rows.next_chunk(8).unwrap(), output_rows);
    assert!(rows.next_chunk(8).unwrap().is_empty());

    let restored = FileProgram::builder("analysis")
        .schema(sample_schema())
        .file_store(store)
        .build_file()
        .unwrap();
    assert_eq!(restored.read_relation("Output").unwrap().rows().len(), 2);
}

#[test]
fn file_program_iter_relation_decodes_rows_lazily() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = FileRelationStore::new(tempdir.path().join("runtime"));
    let output_rows = vec![
        Row::new(vec![
            Value::Number(6),
            Value::Record(vec![Value::Unsigned(12), Value::Float(-0.0)]),
        ]),
        Row::new(vec![
            Value::Number(7),
            Value::Record(vec![Value::Unsigned(13), Value::Float(4.5)]),
        ]),
    ];

    let mut program = FileProgram::builder("analysis")
        .schema(sample_schema())
        .file_store(store.clone())
        .build_file()
        .unwrap();
    program
        .replace_relation_rows("Output", output_rows.clone())
        .unwrap();

    let manifest = store.load_manifest().unwrap();
    let artifact = manifest
        .relations
        .iter()
        .find(|artifact| artifact.relation == "Output")
        .unwrap();
    let first_row_json = serde_json::to_string(&output_rows[0]).unwrap();
    fs::write(
        store.root().join(&artifact.rows_path),
        format!("{first_row_json}\nnot-json\n"),
    )
    .unwrap();

    let mut rows = program.iter_relation("Output").unwrap();
    assert_eq!(rows.next_row().unwrap(), Some(output_rows[0].clone()));
    assert!(matches!(
        rows.next_row().unwrap_err(),
        SouffleError::ArtifactDecodeFailed { .. }
    ));
}

#[test]
fn file_program_requires_store_configuration() {
    let error = FileProgram::builder("analysis")
        .schema(sample_schema())
        .build_file()
        .unwrap_err();

    assert_eq!(
        error,
        SouffleError::BackendConfiguration {
            backend: Backend::File,
            field: "file_store".to_owned(),
            message: "missing FileRelationStore".to_owned(),
        }
    );
}

#[test]
#[cfg(feature = "sqlite")]
fn sqlite_relation_store_exports_schema_and_rows() {
    let mut program = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let exact_nan = f64::from_bits(0x7ff8_0000_0000_0456);
    program
        .replace_relation_rows(
            "Output",
            [Row::new(vec![
                Value::Number(2),
                Value::Record(vec![Value::Unsigned(9), Value::Float(exact_nan)]),
            ])],
        )
        .unwrap();

    let tempdir = tempfile::tempdir().unwrap();
    let store = SqliteRelationStore::new(tempdir.path().join("nested/relations.sqlite"));
    let artifacts = store.export_outputs(&program, ["Output"]).unwrap();

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].relation, "Output");
    assert_eq!(artifacts[0].relation_id, 1);
    assert_eq!(artifacts[0].row_count, 1);
    assert!(store.path().exists());

    let loaded_artifacts = store.load_artifacts().unwrap();
    assert_eq!(loaded_artifacts, artifacts);

    let loaded = store.load_outputs().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].schema().name(), "Output");
    assert_nested_float_bits(&loaded[0].rows()[0], 0x7ff8_0000_0000_0456);

    let empty_artifacts = store
        .export_outputs(&program, std::iter::empty::<&str>())
        .unwrap();
    assert!(empty_artifacts.is_empty());
    assert!(store.load_outputs().unwrap().is_empty());
}

#[test]
#[cfg(feature = "sqlite")]
fn sqlite_relation_store_exports_outputs_from_iterator_without_read_relation() {
    let output_rows = vec![
        Row::new(vec![
            Value::Number(4),
            Value::Record(vec![Value::Unsigned(10), Value::Float(-0.0)]),
        ]),
        Row::new(vec![
            Value::Number(5),
            Value::Record(vec![Value::Unsigned(11), Value::Float(2.5)]),
        ]),
    ];
    let program = IteratorOnlyProgram::new(output_rows.clone());
    let tempdir = tempfile::tempdir().unwrap();
    let store = SqliteRelationStore::new(tempdir.path().join("relations.sqlite"));

    let artifacts = store.export_outputs(&program, ["Output"]).unwrap();

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].row_count, output_rows.len());
    assert_eq!(
        store.load_outputs().unwrap()[0].rows(),
        output_rows.as_slice()
    );
}

#[test]
#[cfg(feature = "sqlite")]
fn sqlite_program_persists_dynamic_rows_and_iterates_outputs() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = SqliteRelationStore::new(tempdir.path().join("runtime/relations.sqlite"));
    let output_rows = vec![
        Row::new(vec![
            Value::Number(4),
            Value::Record(vec![Value::Unsigned(10), Value::Float(-0.0)]),
        ]),
        Row::new(vec![
            Value::Number(5),
            Value::Record(vec![Value::Unsigned(11), Value::Float(2.5)]),
        ]),
    ];

    let mut program = SqliteProgram::builder("analysis")
        .schema(sample_schema())
        .sqlite_store(store.clone())
        .build_sqlite()
        .unwrap();
    program
        .insert_row("Input", [Value::Number(9), Value::Symbol("persist".into())])
        .unwrap();
    program
        .replace_relation_rows("Output", output_rows.clone())
        .unwrap();
    let run_options = RunOptions::new(NonZeroUsize::new(6).unwrap());
    program.run_with_options(run_options.clone()).unwrap();

    assert_eq!(program.backend(), Backend::Sqlite);
    assert_eq!(program.store().path(), store.path());
    assert_eq!(program.last_run_options(), Some(&run_options));
    assert!(store.path().exists());

    let artifacts = store.load_artifacts().unwrap();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(
        artifacts
            .iter()
            .find(|artifact| artifact.relation == "Input")
            .unwrap()
            .row_count,
        1
    );
    assert_eq!(
        artifacts
            .iter()
            .find(|artifact| artifact.relation == "Output")
            .unwrap()
            .row_count,
        2
    );

    let mut rows = program.iter_relation("Output").unwrap();
    assert_eq!(rows.next_chunk(8).unwrap(), output_rows);
    assert!(rows.next_chunk(8).unwrap().is_empty());

    let restored = SqliteProgram::builder("analysis")
        .schema(sample_schema())
        .sqlite_store(store)
        .build_sqlite()
        .unwrap();
    assert_eq!(restored.read_relation("Output").unwrap().rows().len(), 2);
}

#[test]
#[cfg(feature = "sqlite")]
fn sqlite_program_iter_relation_decodes_rows_lazily() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = SqliteRelationStore::new(tempdir.path().join("runtime/relations.sqlite"));
    let output_rows = vec![
        Row::new(vec![
            Value::Number(4),
            Value::Record(vec![Value::Unsigned(10), Value::Float(-0.0)]),
        ]),
        Row::new(vec![
            Value::Number(5),
            Value::Record(vec![Value::Unsigned(11), Value::Float(2.5)]),
        ]),
    ];

    let mut program = SqliteProgram::builder("analysis")
        .schema(sample_schema())
        .sqlite_store(store.clone())
        .build_sqlite()
        .unwrap();
    program
        .replace_relation_rows("Output", output_rows.clone())
        .unwrap();

    let connection = rusqlite::Connection::open(store.path()).unwrap();
    connection
        .execute(
            "UPDATE relation_rows SET row_json = 'not-json' \
             WHERE relation = 'Output' AND row_index = 1",
            [],
        )
        .unwrap();

    let mut rows = program.iter_relation("Output").unwrap();
    assert_eq!(rows.next_row().unwrap(), Some(output_rows[0].clone()));
    assert!(matches!(
        rows.next_row().unwrap_err(),
        SouffleError::ArtifactDecodeFailed { .. }
    ));
}

#[test]
#[cfg(feature = "sqlite")]
fn sqlite_program_requires_store_configuration() {
    let error = SqliteProgram::builder("analysis")
        .schema(sample_schema())
        .build_sqlite()
        .unwrap_err();

    assert_eq!(
        error,
        SouffleError::BackendConfiguration {
            backend: Backend::Sqlite,
            field: "sqlite_store".to_owned(),
            message: "missing SqliteRelationStore".to_owned(),
        }
    );
}

#[test]
fn row_json_preserves_float_bits_for_signed_zero_and_nan() {
    let row = Row::new(vec![
        Value::Float(-0.0),
        Value::Float(f64::from_bits(0x7ff8_0000_0000_abcd)),
    ]);

    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains("8000000000000000"));
    assert!(json.contains("7ff800000000abcd"));

    let decoded: Row = serde_json::from_str(&json).unwrap();
    assert_float_bits(decoded.values()[0].clone(), 0x8000_0000_0000_0000);
    assert_float_bits(decoded.values()[1].clone(), 0x7ff8_0000_0000_abcd);
}

#[test]
fn row_json_preserves_declared_type_wrappers() {
    let row = Row::new([Value::typed("Small", Value::Number(7))]);

    assert_eq!(row.values()[0].kind(), ValueKind::Number);
    assert_eq!(row.values()[0].declared_type_name(), Some("Small"));
    assert_eq!(row.values()[0].untyped(), &Value::Number(7));

    let json = serde_json::to_string(&row).unwrap();
    assert!(json.contains("typed"));
    assert!(json.contains("Small"));

    let decoded: Row = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, row);
    let mut values = decoded.into_values().into_iter();
    assert_eq!(values.next().unwrap().into_untyped(), Value::Number(7));
}

#[test]
fn backend_parity_compares_float_values_by_bits() {
    let nan = f64::from_bits(0x7ff8_0000_0000_0123);
    let mut left = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let mut right = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let row = Row::new(vec![
        Value::Number(1),
        Value::Record(vec![Value::Unsigned(7), Value::Float(nan)]),
    ]);

    left.replace_relation_rows("Output", [row.clone()]).unwrap();
    right.replace_relation_rows("Output", [row]).unwrap();

    verify_backend_parity(&left, &right, ["Output"]).unwrap();
}

#[test]
fn backend_parity_compares_iterators_without_read_relation() {
    let output_rows = vec![
        Row::new(vec![
            Value::Number(1),
            Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
        ]),
        Row::new(vec![
            Value::Number(2),
            Value::Record(vec![Value::Unsigned(8), Value::Float(-0.0)]),
        ]),
    ];
    let left = IteratorOnlyProgram::new(output_rows.clone());
    let right = IteratorOnlyProgram::new(output_rows);

    verify_backend_parity(&left, &right, ["Output"]).unwrap();
}

#[test]
fn backend_parity_reports_schema_normalized_value_mismatch() {
    let mut left = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();
    let mut right = InMemoryProgram::builder("analysis")
        .schema(sample_schema())
        .build_memory();

    left.replace_relation_rows(
        "Output",
        [Row::new(vec![
            Value::Number(1),
            Value::Record(vec![Value::Unsigned(7), Value::Float(0.0)]),
        ])],
    )
    .unwrap();
    right
        .replace_relation_rows(
            "Output",
            [Row::new(vec![
                Value::Number(1),
                Value::Record(vec![Value::Unsigned(7), Value::Float(-0.0)]),
            ])],
        )
        .unwrap();

    let error = verify_backend_parity(&left, &right, ["Output"]).unwrap_err();

    match error {
        SouffleError::BackendParityMismatch { relation, message } => {
            assert_eq!(relation, "Output");
            assert!(message.contains("payload[1] float bits differ"));
            assert!(message.contains("8000000000000000"));
        }
        error => panic!("expected backend parity mismatch, got {error:?}"),
    }
}

fn assert_nested_float_bits(row: &Row, expected: u64) {
    match &row.values()[1] {
        Value::Record(fields) => assert_float_bits(fields[1].clone(), expected),
        value => panic!("expected record payload, got {value:?}"),
    }
}

fn assert_float_bits(value: Value, expected: u64) {
    match value {
        Value::Float(value) => assert_eq!(value.to_bits(), expected),
        value => panic!("expected float, got {value:?}"),
    }
}

fn ffi_error(status: SouffleRsStatus, message: &'static [u8]) -> SouffleRsError {
    SouffleRsError {
        status,
        message: SouffleRsString {
            data: message.as_ptr().cast(),
            len: message.len(),
        },
    }
}

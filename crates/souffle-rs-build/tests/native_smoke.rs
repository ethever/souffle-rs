use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use souffle_rs::{AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef, ValueKind};
use souffle_rs_build::{
    Build, BuildMetadata, FunctorLibrary, GeneratedMode, LinkMode, NativeLinkMode,
};

#[test]
#[ignore = "requires a local Souffle install and cc build-script environment variables"]
fn compiles_generated_scalar_wrapper_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.decl Input(x:number, y:symbol)
.input Input
.decl Output(x:number, y:symbol)
.output Output
Output(x,y) :- Input(x,y).
",
        scalar_schema(),
    ) else {
        return;
    };

    let header = fixture.out_dir.join("include/souffle_rs.h");
    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let library = fixture.out_dir.join("native/libsouffle_rs_generated.a");

    assert_eq!(
        fixture.metadata.c_header_artifact.as_deref(),
        Some(header.as_path())
    );
    assert_eq!(
        fixture.metadata.cxx_wrapper_artifact.as_deref(),
        Some(wrapper.as_path())
    );
    assert!(fixture.metadata.native.wrapper_sources.contains(&wrapper));
    assert!(
        fixture
            .metadata
            .native
            .include_dirs
            .contains(&fixture.out_dir.join("include"))
    );
    assert!(header.exists(), "generated header should exist");
    assert!(wrapper.exists(), "generated wrapper source should exist");
    assert!(library.exists(), "native static library should exist");

    let header_source = fs::read_to_string(&header).expect("read generated header");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(header_source.contains("souffle_rs_program_open_relation_iterator"));
    assert!(header_source.contains("souffle_rs_relation_iterator_next_chunk"));
    assert!(wrapper_source.contains("struct SouffleRsRelationIterator"));
    assert!(wrapper_source.contains("materialize_iterator_chunk"));
    assert!(wrapper_source.contains("souffle_rs_relation_iterator_free(iterator)"));
}

#[test]
#[ignore = "requires a local Souffle install, a C++ compiler, and cc build-script environment variables"]
fn runs_linked_embedded_composite_program_and_matches_process_backend() {
    let Some(fixture) = compile_native_fixture(
        "\
.type Pair = [id:number, label:symbol]
.type List = [head:number, tail:List]
.type Choice = Some {payload:Pair, values:List}
.decl ComplexIn(payload:Pair, values:List, choice:Choice)
.input ComplexIn
.decl ComplexOut(payload:Pair, values:List, choice:Choice)
.output ComplexOut
ComplexOut(payload, values, choice) :- ComplexIn(payload, values, choice).
",
        linked_composite_schema(),
    ) else {
        return;
    };

    let process_executable = fixture.tempdir.path().join("analysis-process");
    compile_souffle_executable(
        &fixture.souffle_bin,
        &fixture.logic_path,
        &process_executable,
    );

    let embedded_executable =
        compile_embedded_runner(&fixture, embedded_composite_runner_source(&fixture));
    let embedded_output = Command::new(&embedded_executable)
        .arg(&process_executable)
        .output()
        .expect("run linked embedded runner");
    assert!(
        embedded_output.status.success(),
        "embedded runner failed with status {}; stdout: {}; stderr: {}",
        embedded_output.status,
        String::from_utf8_lossy(&embedded_output.stdout),
        String::from_utf8_lossy(&embedded_output.stderr)
    );
    let embedded_stdout = String::from_utf8(embedded_output.stdout).expect("embedded stdout utf8");

    assert_eq!(
        embedded_stdout,
        "linked embedded/process/file/sqlite parity rows=1\n"
    );
}

#[test]
#[ignore = "requires a local Souffle install, a C++ compiler, and cc build-script environment variables"]
fn runs_linked_embedded_enum_adt_program_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.type Color = Red {} | Green {}
.decl Input(color:Color)
.input Input
.decl Output(label:symbol, color:Color)
.output Output
Output(\"constant\", $Red()).
Output(\"red\", $Red()) :- Input($Red()).
Output(\"green\", $Green()) :- Input($Green()).
",
        enum_adt_schema(),
    ) else {
        return;
    };

    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(wrapper_source.contains("adt_is_enum"));
    assert!(wrapper_source.contains("enum ADT output used variant index outside schema"));

    let embedded_executable = compile_embedded_runner(&fixture, embedded_enum_adt_runner_source());
    let embedded_output = Command::new(&embedded_executable)
        .output()
        .expect("run linked embedded enum ADT runner");
    assert!(
        embedded_output.status.success(),
        "embedded enum ADT runner failed with status {}; stdout: {}; stderr: {}",
        embedded_output.status,
        String::from_utf8_lossy(&embedded_output.stdout),
        String::from_utf8_lossy(&embedded_output.stderr)
    );
    let embedded_stdout = String::from_utf8(embedded_output.stdout).expect("embedded stdout utf8");

    assert_eq!(embedded_stdout, "enum ADT rows=3\n");
}

#[test]
#[ignore = "requires a local Souffle install, a C++ compiler, and cc build-script environment variables"]
fn compiles_and_runs_custom_functor_with_real_souffle() {
    compile_and_run_custom_functor_with_real_souffle(NativeLinkMode::Dynamic);
}

#[test]
#[ignore = "requires a local Souffle install, a C++ compiler, an archive tool, and cc build-script environment variables"]
fn compiles_and_runs_static_custom_functor_with_real_souffle() {
    compile_and_run_custom_functor_with_real_souffle(NativeLinkMode::Static);
}

fn compile_and_run_custom_functor_with_real_souffle(link_mode: NativeLinkMode) {
    let Some(souffle_bin) = find_souffle_bin() else {
        eprintln!(
            "skipping custom functor smoke: set SOUFFLE_RS_SOUFFLE_BIN or put souffle on PATH"
        );
        return;
    };
    let Some(souffle_include) = find_souffle_include(&souffle_bin) else {
        eprintln!(
            "skipping custom functor smoke: set SOUFFLE_RS_SOUFFLE_INCLUDE or install Souffle headers next to {:?}",
            souffle_bin
        );
        return;
    };
    let missing_env = missing_cc_env();
    if !missing_env.is_empty() {
        eprintln!(
            "skipping custom functor smoke: missing cc build-script env vars: {}",
            missing_env.join(", ")
        );
        return;
    }
    let Some(cxx) = find_cxx_compiler() else {
        eprintln!("skipping custom functor smoke: set CXX or put c++/g++ on PATH");
        return;
    };
    let ar = if link_mode == NativeLinkMode::Static {
        let Some(ar) = find_archive_tool() else {
            eprintln!("skipping static custom functor smoke: set AR or put ar on PATH");
            return;
        };
        Some(ar)
    } else {
        None
    };

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let functor_dir = tempdir.path().join("functors");
    fs::create_dir_all(&functor_dir).expect("create functor dir");
    let functor_source = functor_dir.join("functors.cpp");
    fs::write(
        &functor_source,
        "\
#include <souffle/RamTypes.h>

extern \"C\" souffle::RamDomain plus_one(souffle::RamDomain value) {
    return value + 1;
}
",
    )
    .expect("write functor source");
    let functor_library = match link_mode {
        NativeLinkMode::Dynamic => {
            let functor_library = dynamic_functor_library_path(&functor_dir, "functors");
            compile_shared_functor_library(
                &cxx,
                &souffle_include,
                &functor_source,
                &functor_library,
            );
            functor_library
        }
        NativeLinkMode::Static => {
            let functor_library = functor_dir.join("libfunctors.a");
            compile_static_functor_library(
                &cxx,
                ar.as_deref()
                    .expect("archive tool checked for static smoke"),
                &souffle_include,
                &functor_source,
                &functor_dir.join("functors.o"),
                &functor_library,
            );
            functor_library
        }
    };
    assert!(
        functor_library.exists(),
        "custom functor library should exist"
    );

    let logic_dir = tempdir.path().join("logic");
    fs::create_dir_all(&logic_dir).expect("create logic dir");
    let logic_path = logic_dir.join("analysis.dl");
    fs::write(
        &logic_path,
        "\
.functor plus_one(value:number):number
.decl Input(value:number)
.input Input
.decl Output(value:number)
.output Output
Output(@plus_one(value)) :- Input(value).
",
    )
    .expect("write Souffle program");

    let out_dir = tempdir.path().join("out");
    let mut build = Build::new()
        .program("analysis", &logic_path)
        .souffle_bin(&souffle_bin)
        .souffle_include(&souffle_include)
        .generated_namespace("analysis_ns")
        .generated_mode(GeneratedMode::SingleFile)
        .out_dir(&out_dir)
        .emit_cxx_wrapper(true)
        .emit_schema(true)
        .schema_bundle("analysis", custom_functor_schema())
        .functor_library(
            FunctorLibrary::new("functors")
                .search_path(&functor_dir)
                .link_mode(link_mode),
        );
    if link_mode == NativeLinkMode::Static {
        build = build.link_mode(LinkMode::StaticGeneratedAndConfiguredExternal);
    }
    let metadata = build
        .compile_native(true)
        .compile()
        .expect("compile generated Souffle program with custom functor library configured");

    assert_eq!(metadata.libraries.len(), 1);
    assert_eq!(metadata.libraries[0].name, "functors");
    assert_eq!(metadata.libraries[0].link_mode, link_mode);
    assert!(metadata.libraries[0].search_paths.contains(&functor_dir));
    assert!(metadata.native.library_dirs.contains(&functor_dir));
    assert!(
        metadata
            .native
            .link_libraries
            .contains(&"functors".to_owned())
    );
    if link_mode == NativeLinkMode::Static {
        assert_eq!(
            metadata.link_mode,
            LinkMode::StaticGeneratedAndConfiguredExternal
        );
    }
    assert!(out_dir.join("native/libsouffle_rs_generated.a").exists());

    let executable = tempdir.path().join("analysis");
    compile_souffle_executable_with_functor(&souffle_bin, &functor_dir, &logic_path, &executable);
    let facts_dir = tempdir.path().join("facts");
    let output_dir = tempdir.path().join("output");
    fs::create_dir_all(&facts_dir).expect("create facts dir");
    fs::create_dir_all(&output_dir).expect("create output dir");
    fs::write(facts_dir.join("Input.facts"), "41\n").expect("write input facts");

    let mut command = Command::new(&executable);
    command.arg("-F").arg(&facts_dir).arg("-D").arg(&output_dir);
    if link_mode == NativeLinkMode::Dynamic {
        let (library_path_key, library_path_value) = dynamic_library_path(&functor_dir);
        command.env(library_path_key, library_path_value);
    }
    let output = command
        .output()
        .expect("run generated executable with custom functor");
    assert!(
        output.status.success(),
        "custom functor executable failed with status {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("Output.csv")).expect("read functor output"),
        "42\n"
    );
}

#[test]
#[ignore = "requires a local Souffle install and cc build-script environment variables"]
fn compiles_generated_record_wrapper_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.type Pair = [x:number, y:symbol]
.decl Input(x:number, y:symbol)
.input Input
.decl Output(payload:Pair)
.output Output
Output([x,y]) :- Input(x,y).
",
        record_schema(),
    ) else {
        return;
    };

    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let library = fixture.out_dir.join("native/libsouffle_rs_generated.a");

    assert!(library.exists(), "native static library should exist");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(wrapper_source.contains("SchemaTypeKind::Record"));
    assert!(wrapper_source.contains("materialize_record_value"));
    assert!(wrapper_source.contains("program->program->getRecordTable().unpack"));
    assert!(wrapper_source.contains("souffle_rs_relation_output_composite_value"));
    assert!(wrapper_source.contains("souffle_rs_relation_iterator_next(iterator"));
}

#[test]
#[ignore = "requires a local Souffle install and cc build-script environment variables"]
fn compiles_generated_list_wrapper_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.type List = [head:number, tail:List]
.decl Input(x:number)
.input Input
.decl Output(values:List)
.output Output
Output([x, [x + 1, nil]]) :- Input(x).
",
        list_schema(),
    ) else {
        return;
    };

    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let library = fixture.out_dir.join("native/libsouffle_rs_generated.a");

    assert!(library.exists(), "native static library should exist");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(wrapper_source.contains("SchemaTypeKind::List"));
    assert!(wrapper_source.contains("materialize_list_value"));
    assert!(!wrapper_source.contains("output list traversal is not implemented"));
}

#[test]
#[ignore = "requires a local Souffle install and cc build-script environment variables"]
fn compiles_generated_multi_variant_adt_wrapper_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.type Expr = Const { value:number } | Name { name:symbol } | Pair { left:number, right:number }
.decl Input(x:number, s:symbol)
.input Input
.decl Output(expr:Expr)
.output Output
Output($Const(x)) :- Input(x, _).
Output($Name(s)) :- Input(_, s).
Output($Pair(x, x + 1)) :- Input(x, _).
",
        adt_schema(),
    ) else {
        return;
    };

    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let library = fixture.out_dir.join("native/libsouffle_rs_generated.a");

    assert!(library.exists(), "native static library should exist");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(wrapper_source.contains("SchemaTypeKind::Adt"));
    assert!(wrapper_source.contains("SchemaAdtVariant"));
    assert!(wrapper_source.contains("materialize_adt_value"));
    assert!(wrapper_source.contains("adt_variants_ordered"));
    assert!(
        wrapper_source.contains("record table returned null while unpacking output ADT payload")
    );
    assert!(!wrapper_source.contains("output ADT traversal is not implemented"));
}

#[test]
#[ignore = "requires a local Souffle install and cc build-script environment variables"]
fn compiles_generated_recursive_adt_wrapper_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.type Expr = Const { value:number } | Add { lhs:Expr, rhs:Expr } | Name { name:symbol }
.decl Output(expr:Expr)
.output Output
Output($Add($Const(1), $Name(\"entry\"))).
",
        recursive_adt_schema(),
    ) else {
        return;
    };

    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let library = fixture.out_dir.join("native/libsouffle_rs_generated.a");

    assert!(library.exists(), "native static library should exist");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(wrapper_source.contains("SchemaTypeKind::Reference"));
    assert!(wrapper_source.contains("extern const SchemaType"));
    assert!(wrapper_source.contains("materialize_adt_value"));
    assert!(!wrapper_source.contains("recursive ADT"));
}

#[test]
#[ignore = "requires a local Souffle install and cc build-script environment variables"]
fn compiles_generated_union_wrapper_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.type Small <: number
.type Large <: number
.type Bucket = Small | Large
.decl Input(s:Small, l:Large)
.input Input
.decl Output(value:Bucket)
.output Output
Output(s) :- Input(s, _).
Output(l) :- Input(_, l).
",
        union_schema(),
    ) else {
        return;
    };

    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let library = fixture.out_dir.join("native/libsouffle_rs_generated.a");

    assert!(library.exists(), "native static library should exist");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(wrapper_source.contains("SchemaTypeKind::Union"));
    assert!(wrapper_source.contains("materialize_union_value"));
    assert!(wrapper_source.contains("value_declared_type_name"));
    assert!(wrapper_source.contains("input declared type is not a union variant"));
    assert!(wrapper_source.contains("union schema variants have incompatible runtime tags"));
    assert!(!wrapper_source.contains("output union traversal is not implemented"));
}

#[test]
#[ignore = "requires a local Souffle install and cc build-script environment variables"]
fn compiles_generated_composite_input_wrapper_with_real_souffle() {
    let Some(fixture) = compile_native_fixture(
        "\
.type Pair = [x:number, y:symbol]
.type List = [head:number, tail:List]
.type Choice = Some { value:symbol }
.decl Input(payload:Pair, values:List, choice:Choice)
.input Input
.decl Output(payload:Pair, values:List, choice:Choice)
.output Output
Output(payload, values, choice) :- Input(payload, values, choice).
",
        composite_input_schema(),
    ) else {
        return;
    };

    let wrapper = fixture.out_dir.join("native/souffle_rs_wrapper.cpp");
    let library = fixture.out_dir.join("native/libsouffle_rs_generated.a");

    assert!(library.exists(), "native static library should exist");
    let wrapper_source = fs::read_to_string(&wrapper).expect("read generated wrapper");
    assert!(wrapper_source.contains("pack_input_record_value"));
    assert!(wrapper_source.contains("pack_input_list_value"));
    assert!(wrapper_source.contains("pack_input_adt_value"));
    assert!(wrapper_source.contains("pack_input_schema_value"));
    assert!(!wrapper_source.contains("input composite packing is not implemented"));
}

#[test]
#[ignore = "requires a local Souffle install"]
fn extracts_schema_from_real_souffle_transformed_ast_without_explicit_bundle() {
    let Some(souffle_bin) = find_souffle_bin() else {
        eprintln!(
            "skipping schema extraction smoke: set SOUFFLE_RS_SOUFFLE_BIN or put souffle on PATH"
        );
        return;
    };

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let logic_dir = tempdir.path().join("logic");
    fs::create_dir_all(&logic_dir).expect("create logic dir");
    let logic_path = logic_dir.join("analysis.dl");
    fs::write(
        &logic_path,
        "\
.type Small <: number
.type Large <: number
.type Bucket = Small | Large
.type Pair = [value:unsigned, weight:float]
.type Numbers = [head:number, tail:Numbers]
.type Expr = Lit { value:number } | Name { symbol:symbol }
.decl Input(id:number, label:symbol, payload:Pair, small:Small, large:Large)
.input Input
.decl Trigger()
.input Trigger
.decl Mid(id:number, payload:Pair, choice:Expr, bucket:Bucket, values:Numbers)
.decl Output(id:number, label:symbol, payload:Pair, choice:Expr, small:Small, bucket:Bucket, values:Numbers)
.output Output
Mid(id,payload,$Lit(id),small,[id, [id + 1, nil]]) :- Input(id,_,payload,small,_).
Mid(id,payload,$Name(label),large,[id, [id + 1, nil]]) :- Input(id,label,payload,_,large), Trigger().
Output(id,label,payload,choice,small,bucket,values) :- Input(id,label,payload,small,_), Mid(id,payload,choice,bucket,values).
",
    )
    .expect("write Souffle program");

    let out_dir = tempdir.path().join("out");
    Build::new()
        .program("analysis", &logic_path)
        .souffle_bin(&souffle_bin)
        .generated_namespace("analysis_ns")
        .generated_mode(GeneratedMode::SingleFile)
        .out_dir(&out_dir)
        .emit_schema(true)
        .emit_typed_api(true)
        .compile()
        .expect("compile Souffle program and extract schema metadata");

    let schema_json =
        fs::read_to_string(out_dir.join("schema/analysis.json")).expect("read schema artifact");
    assert!(schema_json.contains("\"Input\""));
    assert!(schema_json.contains("\"Trigger\""));
    assert!(schema_json.contains("\"Mid\""));
    assert!(schema_json.contains("\"Output\""));
    assert!(schema_json.contains("\"intermediate\""));
    assert!(schema_json.contains("\"record\""));
    assert!(schema_json.contains("\"list\""));
    assert!(schema_json.contains("\"adt\""));
    assert!(schema_json.contains("\"Small\""));
    assert!(schema_json.contains("\"Large\""));
    assert!(schema_json.contains("\"union\""));
    assert!(schema_json.contains("\"Bucket\""));
    assert!(schema_json.contains("\"variant_order\""));

    let typed_api =
        fs::read_to_string(out_dir.join("rust/analysis.rs")).expect("read typed API artifact");
    assert!(typed_api.contains("pub struct InputPayload"));
    assert!(typed_api.contains("pub struct TriggerRow"));
    assert!(typed_api.contains("pub struct MidRow"));
    assert!(typed_api.contains("pub struct MidRelation"));
    assert!(typed_api.contains("RelationKind::Intermediate"));
    assert!(typed_api.contains("pub enum OutputChoice"));
    assert!(typed_api.contains("Lit(i64)"));
    assert!(typed_api.contains("Name(String)"));
    assert!(typed_api.contains("pub small: i64"));
    assert!(typed_api.contains("pub bucket: Value"));
    assert!(typed_api.contains("Value::typed(\"Small\""));
    assert!(typed_api.contains("let value = value.into_untyped();"));
    assert!(typed_api.contains("pub values: Vec<i64>"));
    assert!(typed_api.contains("pub fn handle() -> RelationHandle"));
    assert!(typed_api.contains("program.relation_schema_by_handle(&Self::handle())"));
    assert!(typed_api.contains("program.insert_row_by_handle(&Self::handle(), row)"));
    assert!(typed_api.contains("program.iter_relation_by_handle(&Self::handle())"));
}

struct NativeFixture {
    tempdir: tempfile::TempDir,
    souffle_bin: PathBuf,
    logic_path: PathBuf,
    out_dir: PathBuf,
    metadata: BuildMetadata,
}

fn compile_native_fixture(logic_source: &str, schema: RelationBundle) -> Option<NativeFixture> {
    let Some(souffle_bin) = find_souffle_bin() else {
        eprintln!("skipping native smoke: set SOUFFLE_RS_SOUFFLE_BIN or put souffle on PATH");
        return None;
    };
    let Some(souffle_include) = find_souffle_include(&souffle_bin) else {
        eprintln!(
            "skipping native smoke: set SOUFFLE_RS_SOUFFLE_INCLUDE or install Souffle headers next to {:?}",
            souffle_bin
        );
        return None;
    };
    let missing_env = missing_cc_env();
    if !missing_env.is_empty() {
        eprintln!(
            "skipping native smoke: missing cc build-script env vars: {}",
            missing_env.join(", ")
        );
        return None;
    }

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let logic_dir = tempdir.path().join("logic");
    fs::create_dir_all(&logic_dir).expect("create logic dir");
    let logic_path = logic_dir.join("analysis.dl");
    fs::write(&logic_path, logic_source).expect("write Souffle program");

    let out_dir = tempdir.path().join("out");
    let metadata = Build::new()
        .program("analysis", &logic_path)
        .souffle_bin(&souffle_bin)
        .souffle_include(&souffle_include)
        .generated_namespace("analysis_ns")
        .generated_mode(GeneratedMode::SingleFile)
        .out_dir(&out_dir)
        .emit_cxx_wrapper(true)
        .emit_schema(true)
        .emit_typed_api(true)
        .schema_bundle("analysis", schema)
        .compile_native(true)
        .compile()
        .expect("compile generated Souffle program and C ABI wrapper");

    Some(NativeFixture {
        tempdir,
        souffle_bin,
        logic_path,
        out_dir,
        metadata,
    })
}

fn compile_embedded_runner(fixture: &NativeFixture, source: impl AsRef<str>) -> PathBuf {
    let runner_source = fixture.tempdir.path().join("embedded_runner.rs");
    let runner_executable = fixture.tempdir.path().join("embedded_runner");
    fs::write(&runner_source, source.as_ref()).expect("write embedded runner source");

    let deps_dir = env::current_exe()
        .expect("current test executable")
        .parent()
        .expect("test executable parent")
        .to_path_buf();
    let souffle_rs_rlib = newest_rlib(&deps_dir, "libsouffle_rs-");

    let mut command = Command::new("rustc");
    command
        .arg("--edition=2024")
        .arg(&runner_source)
        .arg("--extern")
        .arg(format!("souffle_rs={}", souffle_rs_rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps_dir.display()))
        .arg("-L")
        .arg(format!(
            "native={}",
            fixture.out_dir.join("native").display()
        ))
        .arg("-l")
        .arg("static=souffle_rs_generated")
        .arg("-l")
        .arg(format!("dylib={}", cxx_runtime_library()))
        .arg("-o")
        .arg(&runner_executable);

    if cfg!(target_os = "linux") {
        command.arg("-C").arg("link-arg=-pthread");
    }

    let output = command.output().expect("run rustc over embedded runner");
    assert!(
        output.status.success(),
        "embedded runner failed to compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    runner_executable
}

fn newest_rlib(deps_dir: &Path, prefix: &str) -> PathBuf {
    fs::read_dir(deps_dir)
        .expect("read target deps")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix) && name.ends_with(".rlib"))
        })
        .max_by_key(|path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .unwrap_or_else(|| panic!("find rlib with prefix {prefix}"))
}

fn cxx_runtime_library() -> &'static str {
    if cfg!(target_os = "macos") {
        "c++"
    } else {
        "stdc++"
    }
}

fn rust_string_literal(value: &str) -> String {
    format!("{value:?}")
}

fn embedded_enum_adt_runner_source() -> &'static str {
    r#"
use std::num::NonZeroUsize;

use souffle_rs::{
    AttributeSchema, EmbeddedProgram, Program, RelationBundle, RelationId, RelationSchema, Row,
    TypeRef, Value,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut program = EmbeddedProgram::builder("analysis")
        .schema(enum_adt_schema())
        .threads(NonZeroUsize::new(2).unwrap())
        .build_embedded()?;
    program.insert_row(
        "Input",
        [Value::Adt {
            variant: "Red".to_owned(),
            fields: Vec::new(),
        }],
    )?;
    program.insert_row(
        "Input",
        [Value::Adt {
            variant: "Green".to_owned(),
            fields: Vec::new(),
        }],
    )?;
    program.run()?;

    let output = program.read_relation("Output")?;
    let mut rows = output.rows().iter().map(row_signature).collect::<Vec<_>>();
    rows.sort();
    assert_eq!(rows, ["constant:Red", "green:Green", "red:Red"]);

    println!("enum ADT rows={}", rows.len());
    Ok(())
}

fn row_signature(row: &Row) -> String {
    match row.values() {
        [Value::Symbol(label), Value::Adt { variant, fields }] if fields.is_empty() => {
            format!("{label}:{variant}")
        }
        values => panic!("unexpected enum ADT output row: {values:?}"),
    }
}

fn enum_adt_schema() -> RelationBundle {
    let color = TypeRef::adt(
        "Color",
        [("Green".to_owned(), Vec::new()), ("Red".to_owned(), Vec::new())],
    );

    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [AttributeSchema::new("color", color.clone())],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [
                AttributeSchema::new("label", TypeRef::Symbol),
                AttributeSchema::new("color", color),
            ],
        ),
    ]
    .into_iter()
    .collect()
}
"#
}

fn embedded_composite_runner_source(fixture: &NativeFixture) -> String {
    let source = r#"
use std::num::NonZeroUsize;
use std::path::PathBuf;

use souffle_rs::{
    verify_backend_parity, AttributeSchema, EmbeddedProgram, FileProgram, FileRelationStore,
    ProcessConfig, ProcessProgram, Program, RelationBundle, RelationId, RelationSchema,
    SqliteProgram, SqliteRelationStore, TypeRef, Value,
};

#[path = __TYPED_API_PATH__]
mod analysis;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let process_executable = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("missing process executable argument")?;
    let schema = linked_composite_schema();

    let mut program = EmbeddedProgram::builder("analysis")
        .schema(schema.clone())
        .threads(NonZeroUsize::new(2).unwrap())
        .build_embedded()?;
    insert_typed_input(&mut program)?;
    program.run()?;
    assert_eq!(analysis::ComplexOutRelation::read(&program)?, expected_typed_output());

    let work_dir = std::env::temp_dir().join(format!(
        "souffle-rs-linked-process-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work_dir);
    let mut process = ProcessProgram::builder("analysis")
        .schema(schema.clone())
        .process_config(ProcessConfig::new(process_executable, &work_dir))
        .build_process()?;
    insert_inputs(&mut process)?;
    process.run()?;

    verify_backend_parity(&program, &process, ["ComplexOut"])?;
    assert_eq!(program.iter_relation("ComplexOut")?.next_chunk(8)?.len(), 1);

    let rows = program.read_relation("ComplexOut")?;
    let output_rows = rows.rows().to_vec();

    let mut file = FileProgram::builder("analysis")
        .schema(schema.clone())
        .file_store(FileRelationStore::new(work_dir.join("file")))
        .build_file()?;
    file.replace_relation_rows("ComplexOut", output_rows.clone())?;
    verify_backend_parity(&program, &file, ["ComplexOut"])?;

    let mut sqlite = SqliteProgram::builder("analysis")
        .schema(schema)
        .sqlite_store(SqliteRelationStore::new(work_dir.join("relations.db")))
        .build_sqlite()?;
    sqlite.replace_relation_rows("ComplexOut", output_rows)?;
    verify_backend_parity(&program, &sqlite, ["ComplexOut"])?;

    println!("linked embedded/process/file/sqlite parity rows={}", rows.rows().len());
    Ok(())
}

fn linked_composite_schema() -> RelationBundle {
    let pair = TypeRef::Record(vec![TypeRef::Number, TypeRef::Symbol]);
    let list = TypeRef::List(Box::new(TypeRef::Number));
    let choice = TypeRef::adt(
        "Choice",
        [("Some".to_owned(), vec![pair.clone(), list.clone()])],
    );

    [
        RelationSchema::input(
            RelationId::new(0),
            "ComplexIn",
            [
                AttributeSchema::new("payload", pair.clone()),
                AttributeSchema::new("values", list.clone()),
                AttributeSchema::new("choice", choice.clone()),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "ComplexOut",
            [
                AttributeSchema::new("payload", pair),
                AttributeSchema::new("values", list),
                AttributeSchema::new("choice", choice),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

fn insert_inputs<P: Program>(program: &mut P) -> Result<(), souffle_rs::SouffleError> {
    let payload = Value::Record(vec![Value::Number(7), Value::Symbol("entry".to_owned())]);
    let values = Value::List(vec![Value::Number(1), Value::Number(2)]);
    let choice = Value::Adt {
        variant: "Some".to_owned(),
        fields: vec![payload.clone(), values.clone()],
    };
    program.insert_row("ComplexIn", [payload, values, choice])
}

fn insert_typed_input<P: Program>(program: &mut P) -> Result<(), souffle_rs::SouffleError> {
    analysis::ComplexInRelation::insert(
        program,
        analysis::ComplexInRow {
            payload: analysis::ComplexInPayload {
                field_0: 7,
                field_1: "entry".to_owned(),
            },
            values: vec![1, 2],
            choice: analysis::ComplexInChoice::Some(
                analysis::ComplexInChoiceSomeField0 {
                    field_0: 7,
                    field_1: "entry".to_owned(),
                },
                vec![1, 2],
            ),
        },
    )
}

fn expected_typed_output() -> Vec<analysis::ComplexOutRow> {
    vec![analysis::ComplexOutRow {
        payload: analysis::ComplexOutPayload {
            field_0: 7,
            field_1: "entry".to_owned(),
        },
        values: vec![1, 2],
        choice: analysis::ComplexOutChoice::Some(
            analysis::ComplexOutChoiceSomeField0 {
                field_0: 7,
                field_1: "entry".to_owned(),
            },
            vec![1, 2],
        ),
    }]
}
"#;
    source.replace(
        "__TYPED_API_PATH__",
        &rust_string_literal(
            &fixture
                .out_dir
                .join("rust/analysis.rs")
                .display()
                .to_string(),
        ),
    )
}

fn scalar_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("x", TypeRef::Number),
                AttributeSchema::new("y", TypeRef::Symbol),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [
                AttributeSchema::new("x", TypeRef::Number),
                AttributeSchema::new("y", TypeRef::Symbol),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

fn record_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("x", TypeRef::Number),
                AttributeSchema::new("y", TypeRef::Symbol),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [AttributeSchema::new(
                "payload",
                TypeRef::Record(vec![TypeRef::Number, TypeRef::Symbol]),
            )],
        ),
    ]
    .into_iter()
    .collect()
}

fn list_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [AttributeSchema::new("x", TypeRef::Number)],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [AttributeSchema::new(
                "values",
                TypeRef::List(Box::new(TypeRef::Number)),
            )],
        ),
    ]
    .into_iter()
    .collect()
}

fn adt_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("x", TypeRef::Number),
                AttributeSchema::new("s", TypeRef::Symbol),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [AttributeSchema::new(
                "expr",
                TypeRef::adt(
                    "Expr",
                    [
                        ("Const".to_owned(), vec![TypeRef::Number]),
                        ("Name".to_owned(), vec![TypeRef::Symbol]),
                        ("Pair".to_owned(), vec![TypeRef::Number, TypeRef::Number]),
                    ],
                ),
            )],
        ),
    ]
    .into_iter()
    .collect()
}

fn enum_adt_schema() -> RelationBundle {
    let color = TypeRef::adt(
        "Color",
        [
            ("Green".to_owned(), Vec::new()),
            ("Red".to_owned(), Vec::new()),
        ],
    );

    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [AttributeSchema::new("color", color.clone())],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [
                AttributeSchema::new("label", TypeRef::Symbol),
                AttributeSchema::new("color", color),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

fn recursive_adt_schema() -> RelationBundle {
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
    [RelationSchema::output(
        RelationId::new(0),
        "Output",
        [AttributeSchema::new("expr", expr)],
    )]
    .into_iter()
    .collect()
}

fn union_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new(
                    "s",
                    TypeRef::Subtype {
                        name: "Small".to_owned(),
                        base: Box::new(TypeRef::Number),
                    },
                ),
                AttributeSchema::new(
                    "l",
                    TypeRef::Subtype {
                        name: "Large".to_owned(),
                        base: Box::new(TypeRef::Number),
                    },
                ),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [AttributeSchema::new(
                "value",
                TypeRef::Union {
                    name: "Bucket".to_owned(),
                    variants: vec![
                        TypeRef::Subtype {
                            name: "Small".to_owned(),
                            base: Box::new(TypeRef::Number),
                        },
                        TypeRef::Subtype {
                            name: "Large".to_owned(),
                            base: Box::new(TypeRef::Number),
                        },
                    ],
                },
            )],
        ),
    ]
    .into_iter()
    .collect()
}

fn composite_input_schema() -> RelationBundle {
    let pair = TypeRef::Record(vec![TypeRef::Number, TypeRef::Symbol]);
    let list = TypeRef::List(Box::new(TypeRef::Number));
    let choice = TypeRef::adt("Choice", [("Some".to_owned(), vec![TypeRef::Symbol])]);

    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [
                AttributeSchema::new("payload", pair.clone()),
                AttributeSchema::new("values", list.clone()),
                AttributeSchema::new("choice", choice.clone()),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [
                AttributeSchema::new("payload", pair),
                AttributeSchema::new("values", list),
                AttributeSchema::new("choice", choice),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

fn linked_composite_schema() -> RelationBundle {
    let pair = TypeRef::Record(vec![TypeRef::Number, TypeRef::Symbol]);
    let list = TypeRef::List(Box::new(TypeRef::Number));
    let choice = TypeRef::adt(
        "Choice",
        [("Some".to_owned(), vec![pair.clone(), list.clone()])],
    );

    [
        RelationSchema::input(
            RelationId::new(0),
            "ComplexIn",
            [
                AttributeSchema::new("payload", pair.clone()),
                AttributeSchema::new("values", list.clone()),
                AttributeSchema::new("choice", choice.clone()),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "ComplexOut",
            [
                AttributeSchema::new("payload", pair),
                AttributeSchema::new("values", list),
                AttributeSchema::new("choice", choice),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

fn custom_functor_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "Input",
            [AttributeSchema::new("value", TypeRef::Number)],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Output",
            [AttributeSchema::new("value", TypeRef::Number)],
        ),
    ]
    .into_iter()
    .collect()
}

fn compile_shared_functor_library(
    cxx: &Path,
    souffle_include: &Path,
    source: &Path,
    library: &Path,
) {
    let output = Command::new(cxx)
        .arg("-std=c++17")
        .arg("-fPIC")
        .arg("-shared")
        .arg("-I")
        .arg(souffle_include)
        .arg(source)
        .arg("-o")
        .arg(library)
        .output()
        .expect("spawn C++ compiler for functor library");
    assert!(
        output.status.success(),
        "custom functor compile failed with status {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn compile_static_functor_library(
    cxx: &Path,
    ar: &Path,
    souffle_include: &Path,
    source: &Path,
    object: &Path,
    library: &Path,
) {
    let output = Command::new(cxx)
        .arg("-std=c++17")
        .arg("-fPIC")
        .arg("-I")
        .arg(souffle_include)
        .arg("-c")
        .arg(source)
        .arg("-o")
        .arg(object)
        .output()
        .expect("spawn C++ compiler for static functor object");
    assert!(
        output.status.success(),
        "custom functor object compile failed with status {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let output = Command::new(ar)
        .arg("crs")
        .arg(library)
        .arg(object)
        .output()
        .expect("spawn archive tool for static functor library");
    assert!(
        output.status.success(),
        "custom functor archive failed with status {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dynamic_functor_library_path(directory: &Path, name: &str) -> PathBuf {
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    directory.join(format!("lib{name}.{extension}"))
}

fn compile_souffle_executable_with_functor(
    souffle_bin: &Path,
    functor_dir: &Path,
    logic_path: &Path,
    executable: &Path,
) {
    let output = Command::new(souffle_bin)
        .arg("-L")
        .arg(functor_dir)
        .arg("-lfunctors")
        .arg("-o")
        .arg(executable)
        .arg(logic_path)
        .output()
        .expect("spawn souffle for custom functor executable");
    assert!(
        output.status.success(),
        "souffle custom functor compile failed with status {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn compile_souffle_executable(souffle_bin: &Path, logic_path: &Path, executable: &Path) {
    let output = Command::new(souffle_bin)
        .arg("-o")
        .arg(executable)
        .arg(logic_path)
        .output()
        .expect("spawn souffle for process executable");
    assert!(
        output.status.success(),
        "souffle process compile failed with status {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn dynamic_library_path(path: &Path) -> (&'static str, OsString) {
    let key = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let mut paths = env::var_os(key)
        .map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    paths.insert(0, path.to_path_buf());
    let value = env::join_paths(paths).expect("join dynamic library path");
    (key, value)
}

fn find_souffle_bin() -> Option<PathBuf> {
    env_path("SOUFFLE_RS_SOUFFLE_BIN")
        .or_else(|| env_path("SOUFFLE"))
        .or_else(|| find_on_path("souffle"))
}

fn find_souffle_include(souffle_bin: &Path) -> Option<PathBuf> {
    env_path("SOUFFLE_RS_SOUFFLE_INCLUDE").or_else(|| {
        souffle_bin
            .parent()
            .and_then(Path::parent)
            .map(|prefix| prefix.join("include"))
            .filter(|include| include.join("souffle/SouffleInterface.h").exists())
    })
}

fn find_cxx_compiler() -> Option<PathBuf> {
    env_path("CXX")
        .or_else(|| find_on_path("c++"))
        .or_else(|| find_on_path("g++"))
}

fn find_archive_tool() -> Option<PathBuf> {
    env_path("AR").or_else(|| find_on_path("ar"))
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn find_on_path(binary: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(binary))
        .find(|path| path.is_file())
}

fn missing_cc_env() -> Vec<&'static str> {
    ["TARGET", "HOST", "OUT_DIR", "OPT_LEVEL", "PROFILE"]
        .into_iter()
        .filter(|name| env::var_os(name).is_none())
        .collect()
}

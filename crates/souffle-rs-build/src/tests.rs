use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::Mutex,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::{
    Build, BuildError, BuildProfile, CargoDirective, CppStandard, ExternalLibrary,
    ExternalLibraryKind, FunctorLibrary, GeneratedMode, LinkMode, NativeLinkMode, OpenMpConfig,
    config::{SUPPORTED_SOUFFLE_VERSION, cargo_manifest_path, native_compiler_env_vars},
};
use souffle_rs::{AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef};

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn configured_build() -> Build {
    Build::new()
        .program("analysis", "logic/main.dl")
        .souffle_bin("/opt/souffle/bin/souffle")
        .souffle_version("2.4.1")
        .souffle_include("/opt/souffle/include")
        .generated_namespace("analysis_ns")
        .define("PROJECT_DIR", "/workspace/project")
        .include_dir("logic/include")
        .wrapper_source("native/wrapper.cpp")
        .library_dir("souffle-addon")
        .functor_library(
            FunctorLibrary::new("functors")
                .search_path("souffle-functors")
                .link_library("functor_dep")
                .link_mode(NativeLinkMode::Static),
        )
        .external_library(
            ExternalLibrary::addon("souffle_addon")
                .search_path("souffle-addon-lib")
                .link_mode(NativeLinkMode::Dynamic),
        )
        .external_library(
            ExternalLibrary::z3("z3")
                .search_path("z3-lib")
                .link_mode(NativeLinkMode::Static),
        )
        .external_library(ExternalLibrary::zlib("z").link_mode(NativeLinkMode::Dynamic))
        .external_library(ExternalLibrary::sqlite("sqlite3").link_mode(NativeLinkMode::Dynamic))
        .external_library(ExternalLibrary::cxx_runtime("stdc++").link_mode(NativeLinkMode::Dynamic))
        .compiler("clang++")
        .cpp_standard(CppStandard::Cxx20)
        .target_triple("x86_64-apple-darwin")
        .openmp(OpenMpConfig::enabled("gomp"))
        .link_mode(LinkMode::StaticGeneratedAndConfiguredExternal)
        .rpath("/opt/souffle/lib")
        .install_name("@rpath/libanalysis.dylib")
        .emit_schema(true)
        .emit_typed_api(true)
}

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
                AttributeSchema::new("numbers", TypeRef::List(Box::new(TypeRef::Number))),
                AttributeSchema::new(
                    "choice",
                    TypeRef::adt(
                        "Choice",
                        [
                            ("Some".to_owned(), vec![TypeRef::Symbol]),
                            ("Pair".to_owned(), vec![TypeRef::Number, TypeRef::Symbol]),
                            ("None".to_owned(), Vec::new()),
                        ],
                    ),
                ),
                AttributeSchema::new(
                    "nested",
                    TypeRef::Record(vec![
                        TypeRef::List(Box::new(TypeRef::Symbol)),
                        TypeRef::Record(vec![TypeRef::Number]),
                    ]),
                ),
                AttributeSchema::new(
                    "bucket",
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
                ),
            ],
        ),
    ]
    .into_iter()
    .collect()
}

fn rendered_directives(plan: &crate::BuildPlan) -> Vec<String> {
    plan.cargo_directives()
        .iter()
        .map(CargoDirective::render)
        .collect()
}

fn contains_arg_pair(args: &[OsString], flag: &str, value: &str) -> bool {
    let flag = std::ffi::OsStr::new(flag);
    let value = std::ffi::OsStr::new(value);
    args.windows(2)
        .any(|pair| pair[0].as_os_str() == flag && pair[1].as_os_str() == value)
}

fn assert_empty_path_error(build: Build, field: &'static str) {
    assert_eq!(build.plan().unwrap_err(), BuildError::EmptyPath { field });
}

fn assert_empty_value_error(build: Build, field: &'static str) {
    assert_eq!(build.plan().unwrap_err(), BuildError::EmptyValue { field });
}

fn assert_invalid_identifier_error(build: Build, field: &'static str, value: &str) {
    assert_eq!(
        build.plan().unwrap_err(),
        BuildError::InvalidIdentifierValue {
            field,
            value: value.to_owned(),
        }
    );
}

#[test]
fn cargo_env_helpers_hide_generated_output_layout() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tempdir = tempfile::tempdir().unwrap();
    let manifest_dir = tempdir.path().join("package");
    let cargo_out_dir = tempdir.path().join("cargo-out");
    fs::create_dir_all(&manifest_dir).unwrap();
    let _manifest = EnvVarGuard::set("CARGO_MANIFEST_DIR", &manifest_dir);
    let _out = EnvVarGuard::set("OUT_DIR", &cargo_out_dir);

    let logic_path = cargo_manifest_path("logic/reachability.dl").unwrap();
    let metadata = Build::new()
        .out_dir_from_cargo_env()
        .unwrap()
        .program("analysis", &logic_path)
        .metadata()
        .unwrap();

    assert_eq!(logic_path, manifest_dir.join("logic/reachability.dl"));
    assert_eq!(metadata.out_dir, cargo_out_dir.join("souffle-rs"));
}

#[test]
fn out_dir_from_cargo_env_reports_missing_out_dir() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _out = EnvVarGuard::unset("OUT_DIR");

    assert_eq!(
        Build::new().out_dir_from_cargo_env().unwrap_err(),
        BuildError::MissingCargoEnv {
            variable: "OUT_DIR",
        }
    );
}

#[test]
fn directory_mode_plans_souffle_generation_command() {
    let plan = configured_build().plan().unwrap();
    let command = &plan.souffle_commands()[0];
    let args = command.args();

    assert_eq!(command.program(), "analysis");
    assert_eq!(
        command.executable(),
        PathBuf::from("/opt/souffle/bin/souffle")
    );
    assert!(args.contains(&OsString::from("-G")));
    assert!(args.contains(&OsString::from("target/souffle-rs/generated/analysis")));
    assert!(args.contains(&OsString::from("-N")));
    assert!(args.contains(&OsString::from("analysis_ns")));
    assert!(args.contains(&OsString::from("-M")));
    assert!(args.contains(&OsString::from("PROJECT_DIR=/workspace/project")));
    assert!(args.contains(&OsString::from("-I")));
    assert!(args.contains(&OsString::from("logic/include")));
    assert!(contains_arg_pair(args, "-L", "souffle-addon"));
    assert!(contains_arg_pair(args, "-L", "souffle-functors"));
    assert!(contains_arg_pair(args, "-L", "souffle-addon-lib"));
    assert!(contains_arg_pair(args, "-L", "z3-lib"));
    assert!(args.contains(&OsString::from("-lfunctors")));
    assert_eq!(args.last(), Some(&OsString::from("logic/main.dl")));
}

#[test]
fn single_file_mode_plans_stable_cpp_artifact() {
    let plan = configured_build()
        .generated_mode(GeneratedMode::SingleFile)
        .plan()
        .unwrap();
    let args = plan.souffle_commands()[0].args();

    assert!(args.contains(&OsString::from("-g")));
    assert!(args.contains(&OsString::from("target/souffle-rs/generated/analysis.cpp")));
}

#[test]
fn embedded_typed_api_profile_sets_standard_artifacts() {
    let metadata = Build::new()
        .program("analysis", "logic/main.dl")
        .profile(BuildProfile::EmbeddedTypedApi)
        .metadata()
        .unwrap();

    assert_eq!(
        metadata.c_header_artifact,
        Some(PathBuf::from("target/souffle-rs/include/souffle_rs.h"))
    );
    assert_eq!(
        metadata.cxx_wrapper_artifact,
        Some(PathBuf::from(
            "target/souffle-rs/native/souffle_rs_wrapper.cpp"
        ))
    );
    assert_eq!(
        metadata.typed_api_module_artifact,
        Some(PathBuf::from("target/souffle-rs/rust/mod.rs"))
    );
    assert_eq!(
        metadata.programs[0].schema_artifact,
        Some(PathBuf::from("target/souffle-rs/schema/analysis.json"))
    );
    assert_eq!(
        metadata.programs[0].typed_api_artifact,
        Some(PathBuf::from("target/souffle-rs/rust/analysis.rs"))
    );
    assert!(metadata.native.compile_enabled);
    assert_eq!(metadata.native.cpp_standard, CppStandard::Cxx17);
    assert_eq!(
        metadata.native.static_library.as_deref(),
        Some("souffle_rs_generated")
    );
}

#[test]
fn multiple_programs_keep_distinct_namespaces_and_artifacts() {
    let build = Build::new()
        .generated_namespace("fallback::ns")
        .program_with_namespace("analysis", "logic/analysis.dl", "analysis::ns")
        .program_with_namespace("summary", "logic/summary.dl", "summary_ns")
        .emit_schema(true)
        .emit_typed_api(true)
        .emit_typed_api_module(true);
    let plan = build.plan().unwrap();

    assert_eq!(plan.souffle_commands().len(), 2);
    let analysis = &plan.souffle_commands()[0];
    assert_eq!(analysis.program(), "analysis");
    assert!(contains_arg_pair(analysis.args(), "-N", "analysis::ns"));
    assert!(
        analysis
            .args()
            .contains(&OsString::from("target/souffle-rs/generated/analysis"))
    );
    assert_eq!(
        analysis.args().last(),
        Some(&OsString::from("logic/analysis.dl"))
    );

    let summary = &plan.souffle_commands()[1];
    assert_eq!(summary.program(), "summary");
    assert!(contains_arg_pair(summary.args(), "-N", "summary_ns"));
    assert!(
        summary
            .args()
            .contains(&OsString::from("target/souffle-rs/generated/summary"))
    );
    assert_eq!(
        summary.args().last(),
        Some(&OsString::from("logic/summary.dl"))
    );

    let metadata = build.metadata().unwrap();
    assert_eq!(metadata.programs.len(), 2);
    assert_eq!(metadata.programs[0].generated_namespace, "analysis::ns");
    assert_eq!(
        metadata.programs[0].schema_artifact,
        Some(PathBuf::from("target/souffle-rs/schema/analysis.json"))
    );
    assert_eq!(
        metadata.programs[0].typed_api_artifact,
        Some(PathBuf::from("target/souffle-rs/rust/analysis.rs"))
    );
    assert_eq!(metadata.programs[1].generated_namespace, "summary_ns");
    assert_eq!(
        metadata.programs[1].schema_artifact,
        Some(PathBuf::from("target/souffle-rs/schema/summary.json"))
    );
    assert_eq!(
        metadata.programs[1].typed_api_artifact,
        Some(PathBuf::from("target/souffle-rs/rust/summary.rs"))
    );
    assert_eq!(
        metadata.typed_api_module_artifact,
        Some(PathBuf::from("target/souffle-rs/rust/mod.rs"))
    );
    assert!(
        metadata
            .generated_files
            .contains(&PathBuf::from("target/souffle-rs/schema/analysis.json"))
    );
    assert!(
        metadata
            .generated_files
            .contains(&PathBuf::from("target/souffle-rs/rust/summary.rs"))
    );
    assert!(
        metadata
            .generated_files
            .contains(&PathBuf::from("target/souffle-rs/rust/mod.rs"))
    );

    let module_plan = build.plan().unwrap();
    assert!(
        module_plan
            .cargo_directives()
            .contains(&CargoDirective::RustcEnv {
                key: "SOUFFLE_RS_TYPED_API_MODULE".to_owned(),
                value: std::env::current_dir()
                    .unwrap()
                    .join("target/souffle-rs/rust/mod.rs")
                    .display()
                    .to_string(),
            })
    );
}

#[test]
fn cargo_directives_cover_inputs_env_and_link_libraries() {
    let plan = configured_build().plan().unwrap();
    let rendered = rendered_directives(&plan);

    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "/opt/souffle/bin/souffle"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "logic/main.dl"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "logic/include"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "/opt/souffle/include"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "native/wrapper.cpp"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "souffle-addon"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "souffle-functors"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "souffle-addon-lib"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from("z3-lib")))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfEnvChanged("SOUFFLE".to_owned()))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfEnvChanged("CXX".to_owned()))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfEnvChanged("CXXFLAGS".to_owned()))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfEnvChanged("CXXSTDLIB".to_owned()))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfEnvChanged("TARGET".to_owned()))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RerunIfEnvChanged(
                "CXX_x86_64-apple-darwin".to_owned()
            ))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RustcLinkSearch(PathBuf::from(
                "souffle-addon"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RustcLinkSearch(PathBuf::from(
                "souffle-functors"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RustcLinkSearch(PathBuf::from(
                "souffle-addon-lib"
            )))
    );
    assert!(
        plan.cargo_directives()
            .contains(&CargoDirective::RustcLinkSearch(PathBuf::from("z3-lib")))
    );
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=functors".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=functor_dep".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=souffle_addon".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=z3".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=z".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=sqlite3".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=stdc++".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=gomp".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-arg=-Wl,-rpath,/opt/souffle/lib".to_owned()));
    assert!(
        rendered.contains(
            &"cargo:rustc-link-arg=-Wl,-install_name,@rpath/libanalysis.dylib".to_owned()
        )
    );
    assert!(plan.cargo_directives().contains(&CargoDirective::RustcEnv {
        key: "SOUFFLE_RS_RPATH".to_owned(),
        value: "/opt/souffle/lib".to_owned(),
    }));

    let explicit_compiler_plan = configured_build()
        .compiler("/usr/bin/clang++")
        .plan()
        .unwrap();
    assert!(
        explicit_compiler_plan
            .cargo_directives()
            .contains(&CargoDirective::RerunIfChanged(PathBuf::from(
                "/usr/bin/clang++"
            )))
    );
}

#[test]
fn native_compiler_env_vars_include_target_overrides() {
    let names = native_compiler_env_vars(Some("x86_64-unknown-linux-gnu"));

    assert!(names.contains(&"CXX".to_owned()));
    assert!(names.contains(&"CXX_x86_64-unknown-linux-gnu".to_owned()));
    assert!(names.contains(&"CXX_x86_64_unknown_linux_gnu".to_owned()));
    assert!(names.contains(&"CXXFLAGS_x86_64_unknown_linux_gnu".to_owned()));
    assert!(names.contains(&"CXXSTDLIB_x86_64_unknown_linux_gnu".to_owned()));
    assert!(names.contains(&"CC_x86_64_unknown_linux_gnu".to_owned()));
    assert!(names.contains(&"TARGET".to_owned()));
}

#[test]
fn dynamic_link_mode_forces_external_link_directives_dynamic() {
    let plan = configured_build()
        .link_mode(LinkMode::Dynamic)
        .plan()
        .unwrap();
    let rendered = rendered_directives(&plan);

    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=functors".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=functor_dep".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=souffle_addon".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=z3".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=z".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=sqlite3".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=stdc++".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=dylib=gomp".to_owned()));
    assert!(
        !rendered
            .iter()
            .any(|line| line == "cargo:rustc-link-lib=static=functors")
    );
}

#[test]
fn static_all_link_mode_forces_external_link_directives_static() {
    let plan = configured_build()
        .link_mode(LinkMode::StaticAll)
        .plan()
        .unwrap();
    let rendered = rendered_directives(&plan);

    assert!(rendered.contains(&"cargo:rustc-link-lib=static=functors".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=functor_dep".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=souffle_addon".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=z3".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=z".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=sqlite3".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=stdc++".to_owned()));
    assert!(rendered.contains(&"cargo:rustc-link-lib=static=gomp".to_owned()));
    assert!(
        !rendered
            .iter()
            .any(|line| line == "cargo:rustc-link-lib=dylib=gomp")
    );
}

#[test]
fn metadata_records_reproducible_build_settings() {
    let metadata = configured_build().metadata().unwrap();
    let program = &metadata.programs[0];

    assert_eq!(metadata.out_dir, PathBuf::from("target/souffle-rs"));
    assert_eq!(metadata.c_header_artifact, None);
    assert_eq!(metadata.cxx_wrapper_artifact, None);
    assert_eq!(
        metadata.souffle_bin,
        PathBuf::from("/opt/souffle/bin/souffle")
    );
    assert_eq!(
        metadata.souffle_include,
        Some(PathBuf::from("/opt/souffle/include"))
    );
    assert_eq!(metadata.souffle_version.as_deref(), Some("2.4.1"));
    assert_eq!(metadata.generated_mode, GeneratedMode::Directory);
    assert_eq!(
        metadata.wrapper_source,
        Some(PathBuf::from("native/wrapper.cpp"))
    );
    assert_eq!(
        metadata.link_mode,
        LinkMode::StaticGeneratedAndConfiguredExternal
    );
    assert_eq!(
        metadata.macros.get("PROJECT_DIR").map(String::as_str),
        Some("/workspace/project")
    );
    assert!(
        metadata
            .library_dirs
            .contains(&PathBuf::from("souffle-addon"))
    );
    assert!(
        metadata
            .library_dirs
            .contains(&PathBuf::from("souffle-functors"))
    );
    assert!(
        metadata
            .library_dirs
            .contains(&PathBuf::from("souffle-addon-lib"))
    );
    assert!(metadata.library_dirs.contains(&PathBuf::from("z3-lib")));
    assert_eq!(metadata.openmp.runtime.as_deref(), Some("gomp"));
    assert_eq!(metadata.libraries.len(), 6);
    assert_eq!(metadata.libraries[0].name, "functors");
    assert_eq!(
        metadata.libraries[0].kind,
        ExternalLibraryKind::CustomFunctor
    );
    assert_eq!(metadata.libraries[0].link_mode, NativeLinkMode::Static);
    assert_eq!(
        metadata.libraries[0].link_libraries,
        vec!["functor_dep".to_owned()]
    );
    assert!(
        metadata.libraries[0]
            .search_paths
            .contains(&PathBuf::from("souffle-functors"))
    );
    assert_eq!(metadata.libraries[1].name, "souffle_addon");
    assert_eq!(metadata.libraries[1].kind, ExternalLibraryKind::Addon);
    assert_eq!(metadata.libraries[1].link_mode, NativeLinkMode::Dynamic);
    assert!(
        metadata.libraries[1]
            .search_paths
            .contains(&PathBuf::from("souffle-addon-lib"))
    );
    assert_eq!(metadata.libraries[2].name, "z3");
    assert_eq!(metadata.libraries[2].kind, ExternalLibraryKind::Z3);
    assert_eq!(metadata.libraries[2].link_mode, NativeLinkMode::Static);
    assert!(
        metadata.libraries[2]
            .search_paths
            .contains(&PathBuf::from("z3-lib"))
    );
    assert_eq!(metadata.libraries[3].kind, ExternalLibraryKind::Zlib);
    assert_eq!(metadata.libraries[4].kind, ExternalLibraryKind::Sqlite);
    assert_eq!(metadata.libraries[5].kind, ExternalLibraryKind::CxxRuntime);
    assert_eq!(metadata.native.compiler, Some(PathBuf::from("clang++")));
    assert_eq!(
        metadata.native.target_triple.as_deref(),
        Some("x86_64-apple-darwin")
    );
    assert!(!metadata.native.compile_enabled);
    assert_eq!(metadata.native.static_library, None);
    assert_eq!(metadata.native.cpp_standard, CppStandard::Cxx20);
    assert!(
        metadata
            .native
            .defines
            .contains(&"__EMBEDDED_SOUFFLE__".to_owned())
    );
    assert!(
        metadata
            .native
            .compile_flags
            .contains(&"-std=c++20".to_owned())
    );
    assert!(
        metadata
            .native
            .compile_flags
            .contains(&"-fopenmp".to_owned())
    );
    assert!(
        metadata
            .native
            .include_dirs
            .contains(&PathBuf::from("logic/include"))
    );
    assert!(
        metadata
            .native
            .include_dirs
            .contains(&PathBuf::from("/opt/souffle/include"))
    );
    assert!(
        metadata
            .native
            .library_dirs
            .contains(&PathBuf::from("souffle-addon"))
    );
    assert!(
        metadata
            .native
            .library_dirs
            .contains(&PathBuf::from("souffle-functors"))
    );
    assert!(
        metadata
            .native
            .library_dirs
            .contains(&PathBuf::from("souffle-addon-lib"))
    );
    assert!(
        metadata
            .native
            .library_dirs
            .contains(&PathBuf::from("z3-lib"))
    );
    assert!(
        metadata
            .native
            .link_libraries
            .contains(&"functors".to_owned())
    );
    assert!(
        metadata
            .native
            .link_libraries
            .contains(&"functor_dep".to_owned())
    );
    assert!(
        metadata
            .native
            .link_libraries
            .contains(&"souffle_addon".to_owned())
    );
    assert!(metadata.native.link_libraries.contains(&"z3".to_owned()));
    assert!(metadata.native.link_libraries.contains(&"z".to_owned()));
    assert!(
        metadata
            .native
            .link_libraries
            .contains(&"sqlite3".to_owned())
    );
    assert!(
        metadata
            .native
            .link_libraries
            .contains(&"stdc++".to_owned())
    );
    assert!(metadata.native.link_libraries.contains(&"gomp".to_owned()));
    assert!(
        metadata
            .native
            .wrapper_sources
            .contains(&PathBuf::from("native/wrapper.cpp"))
    );
    assert!(
        metadata
            .native
            .rpaths
            .contains(&PathBuf::from("/opt/souffle/lib"))
    );
    assert_eq!(
        metadata.native.install_name.as_deref(),
        Some("@rpath/libanalysis.dylib")
    );
    assert_eq!(program.program, "analysis");
    assert_eq!(
        program.schema_artifact,
        Some(PathBuf::from("target/souffle-rs/schema/analysis.json"))
    );
    assert_eq!(
        program.typed_api_artifact,
        Some(PathBuf::from("target/souffle-rs/rust/analysis.rs"))
    );

    let json = metadata.to_json_pretty().unwrap();
    assert!(json.contains("\"out_dir\": \"target/souffle-rs\""));
    assert!(json.contains("\"souffle_include\": \"/opt/souffle/include\""));
    assert!(json.contains("\"wrapper_source\": \"native/wrapper.cpp\""));
    assert!(json.contains("\"program\": \"analysis\""));
    assert!(json.contains("\"install_name\": \"@rpath/libanalysis.dylib\""));
    assert!(json.contains("\"abi_version\": 5"));
    let json_value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        json_value["macros"]["PROJECT_DIR"],
        serde_json::Value::String("/workspace/project".to_owned())
    );
    assert!(json_value["macros"].is_object());
    assert_eq!(json_value["generated_mode"], "directory");
    assert_eq!(
        json_value["link_mode"],
        "static-generated-and-configured-external"
    );
    assert_eq!(json_value["openmp"]["link_mode"], "dynamic");
    assert_eq!(json_value["libraries"][0]["kind"], "custom-functor");
    assert_eq!(json_value["libraries"][0]["link_mode"], "static");
    assert_eq!(json_value["libraries"][1]["kind"], "addon");
    assert_eq!(json_value["libraries"][2]["kind"], "z3");
    assert_eq!(json_value["libraries"][3]["kind"], "zlib");
    assert_eq!(json_value["libraries"][4]["kind"], "sqlite");
    assert_eq!(json_value["libraries"][5]["kind"], "cxx-runtime");
    assert_eq!(json_value["native"]["target_triple"], "x86_64-apple-darwin");
    assert_eq!(json_value["native"]["cpp_standard"], "cxx20");
}

#[test]
fn native_compile_opt_in_is_recorded_in_metadata() {
    let metadata = configured_build().compile_native(true).metadata().unwrap();

    assert!(metadata.native.compile_enabled);
    assert_eq!(
        metadata.native.static_library.as_deref(),
        Some("souffle_rs_generated")
    );
}

#[test]
fn c_header_opt_in_is_recorded_in_metadata() {
    let metadata = configured_build().emit_c_header(true).metadata().unwrap();

    assert_eq!(
        metadata.c_header_artifact,
        Some(PathBuf::from("target/souffle-rs/include/souffle_rs.h"))
    );
    assert_eq!(metadata.cxx_wrapper_artifact, None);
}

#[test]
fn cxx_wrapper_opt_in_records_header_wrapper_and_native_inputs() {
    let metadata = configured_build()
        .emit_cxx_wrapper(true)
        .metadata()
        .unwrap();

    assert_eq!(
        metadata.c_header_artifact,
        Some(PathBuf::from("target/souffle-rs/include/souffle_rs.h"))
    );
    assert_eq!(
        metadata.cxx_wrapper_artifact,
        Some(PathBuf::from(
            "target/souffle-rs/native/souffle_rs_wrapper.cpp"
        ))
    );
    assert!(
        metadata
            .native
            .include_dirs
            .contains(&PathBuf::from("target/souffle-rs/include"))
    );
    assert!(
        metadata
            .native
            .wrapper_sources
            .contains(&PathBuf::from("native/wrapper.cpp"))
    );
    assert!(metadata.native.wrapper_sources.contains(&PathBuf::from(
        "target/souffle-rs/native/souffle_rs_wrapper.cpp"
    )));
}

#[test]
fn invalid_program_names_are_typed_errors() {
    let error = Build::new()
        .program("not-valid", "logic/main.dl")
        .plan()
        .unwrap_err();

    assert_eq!(
        error,
        BuildError::InvalidProgramName {
            program: "not-valid".to_owned(),
        }
    );
}

#[test]
fn duplicate_program_names_are_typed_errors() {
    let error = Build::new()
        .program("analysis", "logic/analysis.dl")
        .program_with_namespace("analysis", "logic/other.dl", "other_ns")
        .plan()
        .unwrap_err();

    assert_eq!(
        error,
        BuildError::DuplicateProgramName {
            program: "analysis".to_owned(),
        }
    );
}

#[test]
fn empty_path_settings_are_typed_errors() {
    let build = || Build::new().program("analysis", "logic/main.dl");

    assert_empty_path_error(build().souffle_bin(""), "souffle_bin");
    assert_empty_path_error(build().out_dir(""), "out_dir");
    assert_empty_path_error(build().souffle_include(""), "souffle_include");
    assert_empty_path_error(build().wrapper_source(""), "wrapper_source");
    assert_empty_path_error(build().compiler(""), "compiler");
    assert_empty_path_error(build().include_dir(""), "include_dir");
    assert_empty_path_error(build().library_dir(""), "library_dir");
    assert_empty_path_error(build().rpath(""), "rpath");
    assert_empty_path_error(
        build().functor_library(FunctorLibrary::new("functors").search_path("")),
        "functor_library.search_path",
    );
    assert_empty_path_error(
        build().external_library(ExternalLibrary::z3("z3").search_path("")),
        "external_library.search_path",
    );
    assert_empty_path_error(Build::new().program("analysis", ""), "entrypoint");
}

#[test]
fn empty_string_settings_are_typed_errors() {
    let build = || Build::new().program("analysis", "logic/main.dl");

    assert_empty_value_error(build().generated_namespace(""), "generated_namespace");
    assert_empty_value_error(build().souffle_version(""), "souffle_version");
    assert_empty_value_error(build().define("", "value"), "macro.name");
    assert_empty_value_error(build().define("PROJECT_DIR", ""), "macro.value");
    assert_empty_value_error(
        Build::new().program_with_namespace("analysis", "logic/main.dl", ""),
        "program.generated_namespace",
    );
    assert_empty_value_error(build().install_name(""), "install_name");
    assert_empty_value_error(build().target_triple(""), "target_triple");
    assert_empty_value_error(build().openmp(OpenMpConfig::enabled("")), "openmp.runtime");
    assert_empty_value_error(
        build().openmp(OpenMpConfig::disabled().flag("")),
        "openmp.flag",
    );
    assert_empty_value_error(
        build().functor_library(FunctorLibrary::new("")),
        "functor_library.name",
    );
    assert_empty_value_error(
        build().external_library(ExternalLibrary::new("")),
        "external_library.name",
    );
    assert_empty_value_error(
        build().functor_library(FunctorLibrary::new("functors").link_library("")),
        "functor_library.link_library",
    );
}

#[test]
fn platform_link_args_are_target_aware() {
    let linux_plan = Build::new()
        .program("analysis", "logic/main.dl")
        .target_triple("x86_64-unknown-linux-gnu")
        .rpath("/opt/souffle/lib")
        .plan()
        .unwrap();
    let linux_rendered = rendered_directives(&linux_plan);
    assert!(
        linux_rendered.contains(&"cargo:rustc-link-arg=-Wl,-rpath,/opt/souffle/lib".to_owned())
    );
    assert!(
        !linux_rendered
            .iter()
            .any(|line| line.contains("-install_name"))
    );

    let darwin_plan = Build::new()
        .program("analysis", "logic/main.dl")
        .target_triple("aarch64-apple-darwin")
        .rpath("@loader_path")
        .install_name("@rpath/libanalysis.dylib")
        .plan()
        .unwrap();
    let darwin_rendered = rendered_directives(&darwin_plan);
    assert!(darwin_rendered.contains(&"cargo:rustc-link-arg=-Wl,-rpath,@loader_path".to_owned()));
    assert!(
        darwin_rendered.contains(
            &"cargo:rustc-link-arg=-Wl,-install_name,@rpath/libanalysis.dylib".to_owned()
        )
    );
}

#[test]
fn unsupported_platform_link_capabilities_are_typed_errors() {
    let linux_install_name = Build::new()
        .program("analysis", "logic/main.dl")
        .target_triple("x86_64-unknown-linux-gnu")
        .install_name("@rpath/libanalysis.dylib")
        .plan()
        .unwrap_err();
    assert_eq!(
        linux_install_name,
        BuildError::UnsupportedPlatformCapability {
            capability: "install_name".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
        }
    );

    let unsupported_rpath = Build::new()
        .program("analysis", "logic/main.dl")
        .target_triple("wasm32-unknown-unknown")
        .rpath("/opt/souffle/lib")
        .plan()
        .unwrap_err();
    assert_eq!(
        unsupported_rpath,
        BuildError::UnsupportedPlatformCapability {
            capability: "rpath".to_owned(),
            target: "wasm32-unknown-unknown".to_owned(),
        }
    );
}

#[test]
fn invalid_identifier_settings_are_typed_errors() {
    let build = || Build::new().program("analysis", "logic/main.dl");

    assert_invalid_identifier_error(
        build().generated_namespace("analysis-ns"),
        "generated_namespace",
        "analysis-ns",
    );
    assert_invalid_identifier_error(
        Build::new().program_with_namespace("analysis", "logic/main.dl", "analysis::"),
        "program.generated_namespace",
        "analysis::",
    );
    assert_invalid_identifier_error(
        build().define("PROJECT-DIR", "value"),
        "macro.name",
        "PROJECT-DIR",
    );
}

#[test]
fn compile_runs_souffle_generation_and_writes_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_bin(tempdir.path(), 0);
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, ".decl Input(x:number)\n").unwrap();

    let stale = out_dir.join("generated/analysis/stale.cpp");
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, "stale").unwrap();

    let metadata = Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .emit_c_header(true)
        .emit_cxx_wrapper(true)
        .emit_schema(true)
        .emit_typed_api(true)
        .schema_bundle("analysis", sample_schema())
        .compile()
        .unwrap();

    let generated_marker = out_dir.join("generated/analysis/fake-generated.cpp");
    assert!(generated_marker.exists());
    assert_eq!(
        metadata.souffle_version.as_deref(),
        Some(SUPPORTED_SOUFFLE_VERSION)
    );
    assert_eq!(
        metadata.programs[0].generated_sources,
        vec![generated_marker.clone()]
    );
    assert_eq!(
        metadata.generated_files,
        vec![
            out_dir.join("include/souffle_rs.h"),
            out_dir.join("native/souffle_rs_wrapper.cpp"),
            generated_marker.clone(),
            out_dir.join("schema/analysis.json"),
            out_dir.join("rust/analysis.rs"),
        ]
    );
    assert!(!stale.exists());
    assert_eq!(metadata.metadata_path, out_dir.join("build-metadata.json"));
    assert!(metadata.metadata_path.exists());
    assert!(out_dir.join("schema").is_dir());
    assert!(out_dir.join("rust").is_dir());
    assert!(out_dir.join("include/souffle_rs.h").exists());
    assert!(out_dir.join("native/souffle_rs_wrapper.cpp").exists());

    let metadata_json = fs::read_to_string(&metadata.metadata_path).unwrap();
    assert!(metadata_json.contains("generated_files"));
    assert!(metadata_json.contains(&format!(
        "\"souffle_version\": \"{SUPPORTED_SOUFFLE_VERSION}\""
    )));
    assert!(metadata_json.contains("\"program\": \"analysis\""));
    assert!(metadata_json.contains("generated/analysis"));
    assert!(metadata_json.contains("include/souffle_rs.h"));
    assert!(metadata_json.contains("native/souffle_rs_wrapper.cpp"));

    let c_header = fs::read_to_string(out_dir.join("include/souffle_rs.h")).unwrap();
    assert!(c_header.contains("#define SOUFFLE_RS_ABI_VERSION 5u"));
    assert!(c_header.contains("typedef struct SouffleRsProgram SouffleRsProgram;"));
    assert!(
        c_header.contains("typedef struct SouffleRsRelationIterator SouffleRsRelationIterator;")
    );
    assert!(c_header.contains("typedef struct SouffleRsInputComposite {"));
    assert!(c_header.contains("const SouffleRsInputComposite* composites;"));
    assert!(c_header.contains("size_t composite_count;"));
    assert!(c_header.contains("typedef struct SouffleRsRowOutput {"));
    assert!(c_header.contains("SouffleRsRowOutput* row_output"));
    assert!(c_header.contains("int souffle_rs_program_insert_row("));
    assert!(c_header.contains("int souffle_rs_program_open_relation_iterator("));
    assert!(c_header.contains("int souffle_rs_relation_iterator_next("));
    assert!(c_header.contains("int souffle_rs_relation_iterator_next_chunk("));
    assert!(c_header.contains("void souffle_rs_relation_iterator_free("));
    assert!(c_header.contains("void souffle_rs_row_output_free("));
    assert!(c_header.contains("void* owner;"));
    assert!(c_header.contains("SOUFFLE_RS_VALUE_ADT = 6"));
    assert!(c_header.contains("int souffle_rs_relation_output_composite_len("));
    assert!(c_header.contains("int souffle_rs_relation_output_composite_value("));
    assert!(c_header.contains("int souffle_rs_relation_output_adt_variant("));

    let cxx_wrapper = fs::read_to_string(out_dir.join("native/souffle_rs_wrapper.cpp")).unwrap();
    assert!(cxx_wrapper.contains("SouffleProgram* newInstance_analysis();"));
    assert!(cxx_wrapper.contains("extern \"C\" int souffle_rs_program_insert_row("));
    assert!(cxx_wrapper.contains("struct SouffleRsRelationIterator"));
    assert!(cxx_wrapper.contains("materialize_tuple_row"));
    assert!(cxx_wrapper.contains("materialize_iterator_chunk"));
    assert!(cxx_wrapper.contains("extern \"C\" int souffle_rs_program_open_relation_iterator("));
    assert!(cxx_wrapper.contains("extern \"C\" int souffle_rs_relation_iterator_next("));
    assert!(cxx_wrapper.contains("extern \"C\" int souffle_rs_relation_iterator_next_chunk("));
    assert!(cxx_wrapper.contains("materialize_iterator_row"));
    assert!(cxx_wrapper.contains("SouffleRsRowOutput* row_output"));
    assert!(cxx_wrapper.contains(
        "souffle_rs_program_open_relation_iterator(program, relation_name, &iterator, error)"
    ));
    assert!(
        cxx_wrapper
            .contains("souffle_rs_relation_iterator_next(iterator, &has_row, &output, error)")
    );
    assert!(cxx_wrapper.contains("callback(&output.row, user_data)"));
    assert!(cxx_wrapper.contains("extern \"C\" void souffle_rs_row_output_free("));
    assert!(cxx_wrapper.contains("extern \"C\" void souffle_rs_relation_iterator_free("));
    assert!(cxx_wrapper.contains("relation_output->owner = owner.release();"));
    assert!(cxx_wrapper.contains("pack_input_record_value"));
    assert!(cxx_wrapper.contains("pack_input_list_value"));
    assert!(cxx_wrapper.contains("pack_input_adt_value"));
    assert!(cxx_wrapper.contains("pack_input_union_value"));
    assert!(!cxx_wrapper.contains("input composite packing is not implemented"));
    assert!(cxx_wrapper.contains("struct CompositeNode"));
    assert!(cxx_wrapper.contains("std::vector<CompositeNode> composites;"));
    assert!(cxx_wrapper.contains("static const SchemaType SCHEMA_TYPE_NUMBER"));
    assert!(cxx_wrapper.contains("SchemaTypeKind::Record"));
    assert!(cxx_wrapper.contains("SchemaTypeKind::List"));
    assert!(cxx_wrapper.contains("program->program->getRecordTable().unpack"));
    assert!(cxx_wrapper.contains("materialize_record_value"));
    assert!(cxx_wrapper.contains("materialize_list_value"));
    assert!(cxx_wrapper.contains("struct SchemaAdtVariant"));
    assert!(cxx_wrapper.contains("materialize_adt_value"));
    assert!(cxx_wrapper.contains("materialize_union_value"));
    assert!(cxx_wrapper.contains("union schema variants have incompatible runtime tags"));
    assert!(cxx_wrapper.contains("adt_variants_ordered"));
    assert!(cxx_wrapper.contains("adt_is_enum"));
    assert!(cxx_wrapper.contains("enum ADT output used variant index outside schema"));
    assert!(cxx_wrapper.contains("record table returned null while unpacking output ADT payload"));
    assert!(cxx_wrapper.contains("cyclic list output cannot be decoded"));
    assert!(cxx_wrapper.contains("owner->composites"));
    assert!(cxx_wrapper.contains("schema_type_for_column(program->name"));
    assert!(!cxx_wrapper.contains("output list traversal is not implemented"));
    assert!(!cxx_wrapper.contains("output ADT traversal is not implemented"));
    assert!(cxx_wrapper.contains(
        "ordered ADT variant metadata is required before decoding multi-variant ADT output"
    ));
    assert!(!cxx_wrapper.contains("output composite traversal is not implemented"));

    let schema_json = fs::read_to_string(out_dir.join("schema/analysis.json")).unwrap();
    assert!(schema_json.contains("Input"));
    assert!(schema_json.contains("Choice"));
    assert!(schema_json.contains("Bucket"));
    assert!(schema_json.contains("variant_order"));

    let typed_api_path = out_dir.join("rust/analysis.rs");
    let typed_api = fs::read_to_string(&typed_api_path).unwrap();
    assert_generated_rust_compiles(&typed_api_path);
    assert!(typed_api.contains("pub struct InputRow"));
    assert!(typed_api.contains("pub const PROGRAM_NAME: &str = \"analysis\""));
    assert!(typed_api.contains("pub fn schema_json() -> &'static str"));
    assert!(typed_api.contains("pub fn schema_bundle() -> Result<RelationBundle, SouffleError>"));
    assert!(typed_api.contains("RelationBundle::from_json_str(schema_json())"));
    assert!(typed_api.contains("pub id: i64"));
    assert!(typed_api.contains("pub label: String"));
    assert!(typed_api.contains("impl TryFrom<Row> for InputRow"));
    assert!(typed_api.contains("pub struct OutputPayload"));
    assert!(typed_api.contains("pub field_0: u64"));
    assert!(typed_api.contains("pub field_1: f64"));
    assert!(typed_api.contains("impl From<OutputPayload> for Value"));
    assert!(typed_api.contains("impl TryFrom<Value> for OutputPayload"));
    assert!(typed_api.contains("pub enum OutputChoice"));
    assert!(typed_api.contains("Some(String)"));
    assert!(typed_api.contains("Pair(i64, String)"));
    assert!(typed_api.contains("None"));
    assert!(typed_api.contains("impl From<OutputChoice> for Value"));
    assert!(typed_api.contains("impl TryFrom<Value> for OutputChoice"));
    assert!(typed_api.contains("pub struct OutputNested"));
    assert!(typed_api.contains("pub field_0: Vec<String>"));
    assert!(typed_api.contains("pub field_1: OutputNestedField1"));
    assert!(typed_api.contains("pub struct OutputNestedField1"));
    assert!(typed_api.contains("pub struct OutputRow"));
    assert!(typed_api.contains("pub payload: OutputPayload"));
    assert!(typed_api.contains("pub numbers: Vec<i64>"));
    assert!(typed_api.contains("pub choice: OutputChoice"));
    assert!(typed_api.contains("pub nested: OutputNested"));
    assert!(typed_api.contains("pub bucket: Value"));
    assert!(typed_api.contains("impl TryFrom<Row> for OutputRow"));
    assert!(typed_api.contains("let value = value.into_untyped();"));
    assert!(typed_api.contains("decode_number(\"Output\", \"id\", values.next()"));
    assert!(
        typed_api.contains("decode_record(\"Output\", \"payload\", \"record<unsigned, float>\", 2")
    );
    assert!(
        typed_api.contains("decode_list(\"Output\", \"numbers\", \"list<number>\", values.next()")
    );
    assert!(typed_api.contains("decode_value(\"Output\", \"bucket\", \"Bucket\", &[\"number\"]"));
    assert!(typed_api.contains("pub struct OutputTypedRows<'program>"));
    assert!(typed_api.contains("inner: souffle_rs::RelationIterator<'program>"));
    assert!(typed_api.contains("pub fn next_row(&mut self) -> Result<Option<OutputRow>"));
    assert!(typed_api.contains(
        "pub fn next_chunk(&mut self, max_rows: usize) -> Result<Vec<OutputRow>, SouffleError>"
    ));
    assert!(typed_api.contains(".next_chunk(max_rows)?"));
    assert!(typed_api.contains("pub struct InputRelation"));
    assert!(typed_api.contains("pub fn handle() -> RelationHandle"));
    assert!(typed_api.contains("RelationHandle::new(Self::id(), Self::NAME"));
    assert!(typed_api.contains("pub fn schema<'program, P>("));
    assert!(typed_api.contains("Result<&'program RelationSchema, SouffleError>"));
    assert!(typed_api.contains("program.relation_schema_by_handle(&Self::handle())"));
    assert!(typed_api.contains("program.insert_row_by_handle(&Self::handle(), row)"));
    assert!(typed_api.contains("pub struct OutputRelation"));
    assert!(typed_api.contains("program.iter_relation_by_handle(&Self::handle())"));
    assert!(typed_api.contains("pub fn iter<'program, P>("));
    assert!(typed_api.contains("Result<souffle_rs::RelationIterator<'program>, SouffleError>"));
    assert!(typed_api.contains("pub fn iter_typed<'program, P>("));
    assert!(typed_api.contains("Result<OutputTypedRows<'program>, SouffleError>"));
    assert!(typed_api.contains("pub fn read<P>(program: &P) -> Result<Vec<OutputRow>"));
}

#[test]
fn compile_rejects_unsupported_souffle_version_before_generation() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_version_bin(tempdir.path(), "2.4");
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, ".decl Input(x:number)\n").unwrap();

    let error = Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .compile()
        .unwrap_err();

    assert_eq!(
        error,
        BuildError::UnsupportedSouffleVersion {
            souffle_bin: fake_souffle.display().to_string(),
            expected: SUPPORTED_SOUFFLE_VERSION.to_owned(),
            actual: "2.4".to_owned(),
        }
    );
    assert!(!out_dir.exists());
}

#[test]
fn compile_validates_schema_bundle_before_souffle_generation() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_bin(tempdir.path(), 0);
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, ".decl Output(choice:Choice)\n").unwrap();

    let schema = RelationBundle::from_iter([RelationSchema::output(
        RelationId::new(0),
        "Output",
        [AttributeSchema::new(
            "choice",
            TypeRef::Adt {
                name: "Choice".to_owned(),
                variants: [("Some".to_owned(), vec![TypeRef::Number])]
                    .into_iter()
                    .collect(),
                variant_order: Vec::new(),
                is_enum: false,
            },
        )],
    )]);

    let error = Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .emit_schema(true)
        .schema_bundle("analysis", schema)
        .compile()
        .unwrap_err();

    match error {
        BuildError::SchemaValidation { program, message } => {
            assert_eq!(program, "analysis");
            assert!(message.contains("variant_order"));
        }
        error => panic!("expected schema validation failure, got {error:?}"),
    }
    assert!(
        !out_dir
            .join("generated/analysis/fake-generated.cpp")
            .exists()
    );
    assert!(!out_dir.join("schema/analysis.json").exists());
    assert!(!out_dir.join("build-metadata.json").exists());
}

#[test]
fn compile_emits_typed_api_module_index_for_multiple_programs() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_bin(tempdir.path(), 0);
    let out_dir = tempdir.path().join("out");
    let analysis = tempdir.path().join("logic/analysis.dl");
    let summary = tempdir.path().join("logic/summary.dl");
    fs::create_dir_all(analysis.parent().unwrap()).unwrap();
    fs::write(&analysis, ".decl Input(x:number)\n").unwrap();
    fs::write(&summary, ".decl Input(x:number)\n").unwrap();

    let metadata = Build::new()
        .program_with_namespace("analysis", &analysis, "analysis_ns")
        .program_with_namespace("summary", &summary, "summary_ns")
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .emit_typed_api_module(true)
        .schema_bundle("analysis", sample_schema())
        .schema_bundle("summary", sample_schema())
        .compile()
        .unwrap();

    let module_path = out_dir.join("rust/mod.rs");
    let analysis_api = out_dir.join("rust/analysis.rs");
    let summary_api = out_dir.join("rust/summary.rs");
    assert_eq!(
        metadata.typed_api_module_artifact,
        Some(module_path.clone())
    );
    assert!(metadata.generated_files.contains(&analysis_api));
    assert!(metadata.generated_files.contains(&summary_api));
    assert!(metadata.generated_files.contains(&module_path));

    let module = fs::read_to_string(&module_path).unwrap();
    assert!(module.contains(&format!("#[path = \"{}\"]", analysis_api.display())));
    assert!(module.contains("pub mod analysis;"));
    assert!(module.contains(&format!("#[path = \"{}\"]", summary_api.display())));
    assert!(module.contains("pub mod summary;"));
    assert!(module.contains("pub const PROGRAM_MODULES: &[(&str, &str, &str)]"));
    assert!(module.contains("(\"analysis\", \"analysis\", \"analysis_ns\")"));
    assert!(module.contains("(\"summary\", \"summary\", \"summary_ns\")"));
    assert_generated_rust_compiles(&module_path);
}

fn assert_generated_rust_compiles(path: &Path) {
    let deps_dir = std::env::current_exe()
        .expect("current test executable")
        .parent()
        .expect("test executable parent")
        .to_path_buf();
    let souffle_rs_rlib = fs::read_dir(&deps_dir)
        .expect("read target deps")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("libsouffle_rs-") && name.ends_with(".rlib"))
        })
        .max_by_key(|path| {
            path.metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .expect("find souffle-rs rlib");
    let output = path.with_extension("typed-api-check.rlib");
    let rustc_output = Command::new("rustc")
        .arg("--edition=2024")
        .arg("--crate-type=lib")
        .arg("--crate-name=generated_analysis")
        .arg(path)
        .arg("--extern")
        .arg(format!("souffle_rs={}", souffle_rs_rlib.display()))
        .arg("-L")
        .arg(format!("dependency={}", deps_dir.display()))
        .arg("-o")
        .arg(&output)
        .output()
        .expect("run rustc over generated typed API");

    assert!(
        rustc_output.status.success(),
        "generated typed API failed to compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&rustc_output.stdout),
        String::from_utf8_lossy(&rustc_output.stderr)
    );
}

struct EnvVarGuard {
    name: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: impl AsRef<OsStr>) -> Self {
        let guard = Self {
            name,
            previous: env::var_os(name),
        };
        unsafe {
            env::set_var(name, value);
        }
        guard
    }

    fn unset(name: &'static str) -> Self {
        let guard = Self {
            name,
            previous: env::var_os(name),
        };
        unsafe {
            env::remove_var(name);
        }
        guard
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                env::set_var(self.name, previous);
            } else {
                env::remove_var(self.name);
            }
        }
    }
}

#[test]
fn compile_single_file_mode_creates_parent_and_generated_file() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_bin(tempdir.path(), 0);
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, ".decl Input(x:number)\n").unwrap();

    Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .generated_mode(GeneratedMode::SingleFile)
        .compile()
        .unwrap();

    assert!(out_dir.join("generated/analysis.cpp").exists());
    assert!(out_dir.join("build-metadata.json").exists());
}

#[test]
fn compile_native_reports_missing_sources_before_invoking_compiler() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_without_sources_bin(tempdir.path());
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, ".decl Input(x:number)\n").unwrap();

    let error = Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .compile_native(true)
        .compile()
        .unwrap_err();

    assert_eq!(
        error,
        BuildError::NativeSourcesUnavailable {
            library: "souffle_rs_generated".to_owned(),
        }
    );
    assert!(out_dir.join("generated/analysis").is_dir());
    assert!(!out_dir.join("build-metadata.json").exists());
}

#[test]
fn compile_returns_typed_command_failure() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_bin(tempdir.path(), 17);
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, ".decl Input(x:number)\n").unwrap();

    let error = Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .compile()
        .unwrap_err();

    match error {
        BuildError::CommandFailed(failure) => {
            assert_eq!(failure.program, "analysis");
            assert!(failure.command.contains("fake-souffle"));
            assert_eq!(failure.status, "17");
            assert!(failure.stderr.contains("fake souffle failed"));
        }
        error => panic!("expected command failure, got {error:?}"),
    }
}

#[test]
fn compile_extracts_schema_artifacts_from_transformed_ast() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_with_schema_bin(tempdir.path());
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(
        &entrypoint,
        ".decl Input(id:number, label:symbol)\n.input Input\n",
    )
    .unwrap();

    let metadata = Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .library_dir("souffle-addon")
        .functor_library(FunctorLibrary::new("functors").search_path("souffle-functors"))
        .out_dir(&out_dir)
        .emit_schema(true)
        .emit_typed_api(true)
        .compile()
        .unwrap();

    assert_eq!(
        metadata.programs[0].schema_artifact,
        Some(out_dir.join("schema/analysis.json"))
    );
    assert_eq!(
        metadata.programs[0].typed_api_artifact,
        Some(out_dir.join("rust/analysis.rs"))
    );
    assert!(
        metadata
            .generated_files
            .contains(&out_dir.join("schema/analysis.json"))
    );
    assert!(
        metadata
            .generated_files
            .contains(&out_dir.join("rust/analysis.rs"))
    );
    let schema_json = fs::read_to_string(out_dir.join("schema/analysis.json")).unwrap();
    assert!(schema_json.contains("\"Input\""));
    assert!(schema_json.contains("\"Trigger\""));
    assert!(schema_json.contains("\"Mid\""));
    assert!(schema_json.contains("\"Output\""));
    assert!(schema_json.contains("\"intermediate\""));
    assert!(schema_json.contains("\"record\""));
    assert!(schema_json.contains("\"list\""));
    assert!(schema_json.contains("\"unsigned\""));
    assert!(schema_json.contains("\"float\""));
    assert!(schema_json.contains("\"Small\""));
    assert!(schema_json.contains("\"Large\""));
    assert!(schema_json.contains("\"union\""));
    assert!(schema_json.contains("\"Bucket\""));
    assert!(schema_json.contains("\"Expr\""));
    assert!(schema_json.contains("\"Color\""));
    assert!(schema_json.contains("\"is_enum\": true"));
    assert!(schema_json.contains("\"reference\""));
    assert!(schema_json.contains("\"variant_order\""));

    let typed_api_path = out_dir.join("rust/analysis.rs");
    let typed_api = fs::read_to_string(&typed_api_path).unwrap();
    assert_generated_rust_compiles(&typed_api_path);
    assert!(typed_api.contains("pub struct InputPayload"));
    assert!(typed_api.contains("pub fn schema_bundle() -> Result<RelationBundle, SouffleError>"));
    assert!(typed_api.contains("Add(Value, Value)"));
    assert!(typed_api.contains("pub field_0: u64"));
    assert!(typed_api.contains("pub field_1: f64"));
    assert!(typed_api.contains("pub struct TriggerRow"));
    assert!(typed_api.contains("Row::new(Vec::new())"));
    assert!(typed_api.contains("pub struct MidRow"));
    assert!(typed_api.contains("pub struct MidRelation"));
    assert!(typed_api.contains("RelationKind::Intermediate"));
    assert!(typed_api.contains("pub const PRINTABLE: bool = false"));
    assert!(typed_api.contains("pub enum OutputChoice"));
    assert!(typed_api.contains("Lit(i64)"));
    assert!(typed_api.contains("Name(String)"));
    assert!(typed_api.contains("pub small: i64"));
    assert!(typed_api.contains("pub bucket: Value"));
    assert!(typed_api.contains("Value::typed(\"Small\""));
    assert!(typed_api.contains("let value = value.into_untyped();"));
    assert!(typed_api.contains("decode_value(\"Output\", \"bucket\", \"Bucket\""));
    assert!(typed_api.contains("pub values: Vec<i64>"));
    assert!(typed_api.contains("decode_list(\"Output\", \"values\""));
}

#[test]
fn generated_typed_api_deconflicts_lossy_rust_names() {
    let tempdir = tempfile::tempdir().unwrap();
    let fake_souffle = fake_souffle_bin(tempdir.path(), 0);
    let out_dir = tempdir.path().join("out");
    let entrypoint = tempdir.path().join("logic/main.dl");
    fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
    fs::write(&entrypoint, ".decl FooBar(value:number)\n").unwrap();

    let choice = TypeRef::adt(
        "Choice",
        [
            ("SomeValue".to_owned(), vec![TypeRef::Number]),
            ("Some_Value".to_owned(), vec![TypeRef::Symbol]),
        ],
    );
    let schema = RelationBundle::from_iter([
        RelationSchema::output(
            RelationId::new(0),
            "FooBar",
            [
                AttributeSchema::new("value", TypeRef::Number),
                AttributeSchema::new("Value", TypeRef::Symbol),
                AttributeSchema::new("some_value", TypeRef::Record(vec![choice.clone()])),
            ],
        ),
        RelationSchema::output(
            RelationId::new(1),
            "Foo_Bar",
            [
                AttributeSchema::new("value", TypeRef::Number),
                AttributeSchema::new("somevalue", TypeRef::Record(vec![choice])),
            ],
        ),
    ]);

    let metadata = Build::new()
        .program("analysis", &entrypoint)
        .souffle_bin(&fake_souffle)
        .out_dir(&out_dir)
        .emit_typed_api(true)
        .schema_bundle("analysis", schema)
        .compile()
        .unwrap();

    let typed_api_path = metadata.programs[0]
        .typed_api_artifact
        .as_ref()
        .expect("typed API emitted");
    let typed_api = fs::read_to_string(typed_api_path).unwrap();
    assert_generated_rust_compiles(typed_api_path);
    assert!(typed_api.contains("pub struct FooBarRow"));
    assert!(typed_api.contains("pub struct FooBarRow2"));
    assert!(typed_api.contains("pub value: i64"));
    assert!(typed_api.contains("pub value2: String"));
    assert!(typed_api.contains("SomeValue(i64)"));
    assert!(typed_api.contains("SomeValue2(String)"));
}

fn fake_souffle_bin(root: &Path, exit_code: i32) -> PathBuf {
    let script = root.join("fake-souffle");
    let mut file = fs::File::create(&script).unwrap();
    file.write_all(
        format!(
            r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "--version" ]; then
  printf '%s\n' "Version: {SUPPORTED_SOUFFLE_VERSION}"
  exit 0
fi
if [ "{exit_code}" -ne 0 ]; then
  echo "fake souffle failed" >&2
  exit "{exit_code}"
fi
while [ "$#" -gt 0 ]; do
  case "$1" in
    -G)
      shift
      mkdir -p "$1"
      printf '%s\n' "// generated directory" > "$1/fake-generated.cpp"
      ;;
    -g)
      shift
      mkdir -p "$(dirname "$1")"
      printf '%s\n' "// generated file" > "$1"
      ;;
  esac
  shift || true
done
"#
        )
        .as_bytes(),
    )
    .unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    drop(file);

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
    }

    script
}

fn fake_souffle_version_bin(root: &Path, version: &str) -> PathBuf {
    let script = root.join("fake-souffle-version");
    let mut file = fs::File::create(&script).unwrap();
    file.write_all(
        format!(
            r#"#!/bin/sh
set -eu
if [ "${{1:-}}" = "--version" ]; then
  printf '%s\n' "Version: {version}"
  exit 0
fi
while [ "$#" -gt 0 ]; do
  case "$1" in
    -G)
      shift
      mkdir -p "$1"
      printf '%s\n' "// generated directory" > "$1/fake-generated.cpp"
      ;;
    -g)
      shift
      mkdir -p "$(dirname "$1")"
      printf '%s\n' "// generated file" > "$1"
      ;;
  esac
  shift || true
done
"#
        )
        .as_bytes(),
    )
    .unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    drop(file);

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
    }

    script
}

fn fake_souffle_with_schema_bin(root: &Path) -> PathBuf {
    let script = root.join("fake-souffle-with-schema");
    let mut file = fs::File::create(&script).unwrap();
    let script_source = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  printf '%s\n' "Version: __SOUFFLE_VERSION__"
  exit 0
fi
if [ "${1:-}" = "--show=transformed-ast" ]; then
  args=" $* "
  case "$args" in
    *" -L souffle-addon "*) ;;
    *) echo "missing global library dir in schema extraction command" >&2; exit 23 ;;
  esac
  case "$args" in
    *" -L souffle-functors "*) ;;
    *) echo "missing functor search path in schema extraction command" >&2; exit 24 ;;
  esac
  case "$args" in
    *" -lfunctors "*) ;;
    *) echo "missing functor library in schema extraction command" >&2; exit 25 ;;
  esac
  cat <<'AST'
.type Small <: number
.type Large <: number
.type Bucket = Small | Large
.type Pair = [value:unsigned, weight:float]
.type Numbers = [head:number, tail:Numbers]
.type Expr = Lit { value:number } | Add { lhs:Expr, rhs:Expr } | Name { symbol:symbol }
.type Color = Red {} | Green {}
.decl Input(id:number, label:symbol, payload:Pair)
.decl Trigger()
.decl Mid(payload:Pair, values:Numbers, choice:Expr, bucket:Bucket)
.decl Output(id:number, label:symbol, payload:Pair, choice:Expr, small:Small, bucket:Bucket, values:Numbers, color:Color)
.input Input(IO="file",attributeNames="id	label	payload",fact-dir=".",name="Input",operation="input",params="{"records": {"Pair": {"arity": 2, "params": ["value", "weight"]}}, "relation": {"arity": 3, "params": ["id", "label", "payload"]}}",types="{"ADTs": {}, "records": {"r:Pair": {"arity": 2, "types": ["u:unsigned", "f:float"]}}, "relation": {"arity": 3, "types": ["i:number", "s:symbol", "r:Pair"]}}")
.input Trigger(IO="file",attributeNames="",fact-dir=".",name="Trigger",operation="input",params="{"records": {}, "relation": {"arity": 0, "params": []}}",types="{"ADTs": {}, "records": {}, "relation": {"arity": 0, "types": []}}")
.output Output(IO="file",attributeNames="id	label	payload	choice	small	bucket	values	color",name="Output",operation="output",output-dir=".",params="{"records": {"Numbers": {"arity": 2, "params": ["head", "tail"]}, "Pair": {"arity": 2, "params": ["value", "weight"]}}, "relation": {"arity": 8, "params": ["id", "label", "payload", "choice", "small", "bucket", "values", "color"]}}",types="{"ADTs": {"+:Expr": {"arity": 3, "branches": [{"name": "Lit", "types": ["i:number"]}, {"name": "Add", "types": ["+:Expr", "+:Expr"]}, {"name": "Name", "types": ["s:symbol"]}], "enum": false}, "+:Color": {"arity": 2, "branches": [{"name": "Green", "types": []}, {"name": "Red", "types": []}], "enum": true}}, "records": {"r:Numbers": {"arity": 2, "types": ["i:number", "r:Numbers"]}, "r:Pair": {"arity": 2, "types": ["u:unsigned", "f:float"]}}, "relation": {"arity": 8, "types": ["i:number", "s:symbol", "r:Pair", "+:Expr", "i:Small", "i:Bucket", "r:Numbers", "+:Color"]}}")
AST
  exit 0
fi
while [ "$#" -gt 0 ]; do
  case "$1" in
    -G)
      shift
      mkdir -p "$1"
      printf '%s\n' "// generated directory" > "$1/fake-generated.cpp"
      ;;
    -g)
      shift
      mkdir -p "$(dirname "$1")"
      printf '%s\n' "// generated file" > "$1"
      ;;
  esac
  shift || true
done
"#
    .replace("__SOUFFLE_VERSION__", SUPPORTED_SOUFFLE_VERSION);
    file.write_all(script_source.as_bytes()).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    drop(file);

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
    }

    script
}

fn fake_souffle_without_sources_bin(root: &Path) -> PathBuf {
    let script = root.join("fake-souffle-no-sources");
    let mut file = fs::File::create(&script).unwrap();
    let script_source = r#"#!/bin/sh
set -eu
if [ "${1:-}" = "--version" ]; then
  printf '%s\n' "Version: __SOUFFLE_VERSION__"
  exit 0
fi
while [ "$#" -gt 0 ]; do
  case "$1" in
    -G)
      shift
      mkdir -p "$1"
      ;;
    -g)
      shift
      mkdir -p "$(dirname "$1")"
      ;;
  esac
  shift || true
done
"#
    .replace("__SOUFFLE_VERSION__", SUPPORTED_SOUFFLE_VERSION);
    file.write_all(script_source.as_bytes()).unwrap();
    file.flush().unwrap();
    file.sync_all().unwrap();
    drop(file);

    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();
    }

    script
}

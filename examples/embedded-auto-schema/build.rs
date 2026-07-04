use std::{
    env,
    error::Error,
    io,
    path::{Path, PathBuf},
};

use souffle_rs_build::{Build, CppStandard, GeneratedMode};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-env-changed=SOUFFLE_RS_SOUFFLE_BIN");
    println!("cargo:rerun-if-env-changed=SOUFFLE_RS_SOUFFLE_INCLUDE");

    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "CARGO_MANIFEST_DIR is not set")
        })?);
    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OUT_DIR is not set"))?,
    )
    .join("souffle-rs");
    let logic_path = manifest_dir.join("logic/reachability.dl");
    let souffle_bin = find_souffle_bin();
    let souffle_include = find_souffle_include(&souffle_bin).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "could not find Souffle headers for `{}`; set SOUFFLE_RS_SOUFFLE_INCLUDE",
                souffle_bin.display()
            ),
        )
    })?;

    Build::new()
        .program("reachability", &logic_path)
        .souffle_bin(&souffle_bin)
        .souffle_include(&souffle_include)
        .generated_namespace("reachability_auto_schema_generated")
        .generated_mode(GeneratedMode::SingleFile)
        .cpp_standard(CppStandard::Cxx17)
        .out_dir(&out_dir)
        .emit_c_header(true)
        .emit_cxx_wrapper(true)
        .emit_schema(true)
        .emit_typed_api(true)
        .emit_typed_api_module(true)
        .compile_native(true)
        .compile()?;
    Ok(())
}

fn find_souffle_bin() -> PathBuf {
    env_path("SOUFFLE_RS_SOUFFLE_BIN")
        .or_else(|| env_path("SOUFFLE"))
        .or_else(|| find_on_path("souffle"))
        .unwrap_or_else(|| PathBuf::from("souffle"))
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

use std::{
    env,
    error::Error,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use souffle_rs::{AttributeSchema, RelationBundle, RelationId, RelationSchema, TypeRef};
use souffle_rs_build::{
    Build, BuildProfile, FunctorLibrary, GeneratedMode, NativeLinkMode, cargo_manifest_path,
};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=native/number_addon.cpp");
    println!("cargo:rerun-if-env-changed=SOUFFLE_RS_SOUFFLE_BIN");
    println!("cargo:rerun-if-env-changed=SOUFFLE_RS_SOUFFLE_INCLUDE");
    println!("cargo:rerun-if-env-changed=CXX");

    let logic_path = cargo_manifest_path("logic/addon.dl")?;
    let addon_source = cargo_manifest_path("native/number_addon.cpp")?;
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
    let cxx = find_cxx_compiler().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not find a C++ compiler; set CXX or put c++/g++/clang++ on PATH",
        )
    })?;

    let out_dir = PathBuf::from(
        env::var_os("OUT_DIR")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OUT_DIR is not set"))?,
    );
    let addon_dir = out_dir.join("souffle-addon");
    fs::create_dir_all(&addon_dir)?;
    let addon_library = dynamic_library_path(&addon_dir, "number_addon")?;
    compile_shared_addon_library(&cxx, &souffle_include, &addon_source, &addon_library)?;

    Build::new()
        .out_dir_from_cargo_env()?
        .program("addon_example", &logic_path)
        .souffle_bin(&souffle_bin)
        .souffle_include(&souffle_include)
        .generated_namespace("addon_example_generated")
        .generated_mode(GeneratedMode::SingleFile)
        .schema_bundle("addon_example", addon_schema())
        .functor_library(
            FunctorLibrary::new("number_addon")
                .search_path(&addon_dir)
                .link_mode(NativeLinkMode::Dynamic),
        )
        .profile(BuildProfile::EmbeddedTypedApi)
        .compile()?;
    Ok(())
}

fn addon_schema() -> RelationBundle {
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

fn compile_shared_addon_library(
    cxx: &Path,
    souffle_include: &Path,
    source: &Path,
    library: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut command = Command::new(cxx);
    command
        .arg("-std=c++17")
        .arg("-fPIC")
        .arg(if cfg!(target_os = "macos") {
            "-dynamiclib"
        } else {
            "-shared"
        })
        .arg("-I")
        .arg(souffle_include)
        .arg(source)
        .arg("-o")
        .arg(library);

    let output = command.output().map_err(|source| {
        io::Error::other(format!(
            "failed to spawn C++ compiler `{}`: {source}",
            cxx.display()
        ))
    })?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "addon compile failed with status {}; stdout: {}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
        .into());
    }
    Ok(())
}

fn dynamic_library_path(directory: &Path, name: &str) -> Result<PathBuf, io::Error> {
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the Souffle addon example supports Unix-like dynamic libraries",
        ));
    } else {
        "so"
    };
    Ok(directory.join(format!("lib{name}.{extension}")))
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

fn find_cxx_compiler() -> Option<PathBuf> {
    env_path("CXX")
        .or_else(|| find_on_path("c++"))
        .or_else(|| find_on_path("g++"))
        .or_else(|| find_on_path("clang++"))
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

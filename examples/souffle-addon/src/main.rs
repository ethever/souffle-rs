//! Standalone embedded example that links a Souffle addon library.
//!
//! The `build.rs` in this package compiles `native/number_addon.cpp` into a
//! small dynamic library, passes it to Souffle with
//! `souffle_rs_build::FunctorLibrary`, emits the generated typed API, and links
//! everything into this executable.

mod generated {
    souffle_rs::include_generated_programs!();
}

use souffle_rs::{EmbeddedProgram, Program};

use generated::addon_example;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut program = EmbeddedProgram::builder(addon_example::PROGRAM_NAME)
        .schema(addon_example::schema_bundle()?)
        .build_embedded()?;

    addon_example::InputRelation::insert(&mut program, addon_example::InputRow { value: 41 })?;
    program.run()?;

    let values = addon_example::OutputRelation::read(&program)?
        .into_iter()
        .map(|row| row.value)
        .collect::<Vec<_>>();

    assert_eq!(values, [42]);
    println!("souffle addon output values: 42");
    Ok(())
}

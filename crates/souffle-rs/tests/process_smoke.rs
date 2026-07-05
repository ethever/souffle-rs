use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use souffle_rs::{
    AttributeSchema, ProcessConfig, ProcessProgram, Program, RelationBundle, RelationId,
    RelationSchema, Row, RunOptions, TypeRef, Value,
};

#[test]
#[ignore = "requires a local Souffle install to compile a process executable"]
fn runs_scalar_program_through_process_backend() {
    let Some(souffle_bin) = find_souffle_bin() else {
        eprintln!("skipping process smoke: set SOUFFLE_RS_SOUFFLE_BIN or put souffle on PATH");
        return;
    };

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let logic_path = tempdir.path().join("analysis.dl");
    fs::write(
        &logic_path,
        "\
.decl Input(x:number, y:symbol)
.input Input
.decl Output(x:number, y:symbol)
.output Output
Output(x,y) :- Input(x,y).
",
    )
    .expect("write Souffle program");

    let executable = tempdir.path().join("analysis");
    compile_souffle(&souffle_bin, &logic_path, &executable);

    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(scalar_schema())
        .process_config(ProcessConfig::new(&executable, &work_dir))
        .build_process()
        .expect("build process facade");

    program
        .insert_row(
            "Input",
            [Value::Number(7), Value::Symbol("entry".to_owned())],
        )
        .expect("insert first row");
    program
        .insert_row(
            "Input",
            [Value::Number(11), Value::Symbol("next".to_owned())],
        )
        .expect("insert second row");
    program
        .run_with_options(RunOptions::default())
        .expect("run compiled process");

    assert_eq!(
        fs::read_to_string(work_dir.join("facts/Input.facts")).expect("read facts"),
        "7\tentry\n11\tnext\n"
    );
    assert_eq!(
        program.read_relation("Output").expect("read output").rows(),
        &[
            Row::new(vec![Value::Number(7), Value::Symbol("entry".to_owned())]),
            Row::new(vec![Value::Number(11), Value::Symbol("next".to_owned())]),
        ]
    );
}

#[test]
#[ignore = "requires a local Souffle install to compile a process executable"]
fn runs_scalar_nullary_and_union_program_through_process_backend() {
    let Some(souffle_bin) = find_souffle_bin() else {
        eprintln!("skipping process smoke: set SOUFFLE_RS_SOUFFLE_BIN or put souffle on PATH");
        return;
    };

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let logic_path = tempdir.path().join("analysis.dl");
    fs::write(
        &logic_path,
        "\
.type Small <: number
.type Large <: number
.type Bucket = Small | Large
.decl InputA(id:number, label:symbol, weight:float)
.input InputA
.decl InputB(value:unsigned)
.input InputB
.decl InputSmall(value:Small)
.input InputSmall
.decl InputLarge(value:Large)
.input InputLarge
.decl Trigger()
.input Trigger
.decl OutputA(id:number, label:symbol, value:unsigned, weight:float)
.output OutputA
.decl BucketOut(value:Bucket)
.output BucketOut
.decl Fired()
.output Fired
OutputA(id,label,value,weight) :- InputA(id,label,weight), InputB(value).
BucketOut(value) :- InputSmall(value).
BucketOut(value) :- InputLarge(value).
Fired() :- Trigger().
",
    )
    .expect("write Souffle program");

    let executable = tempdir.path().join("analysis");
    compile_souffle(&souffle_bin, &logic_path, &executable);

    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(process_parity_schema())
        .process_config(ProcessConfig::new(&executable, &work_dir))
        .build_process()
        .expect("build process facade");

    program
        .insert_row(
            "InputA",
            [
                Value::Number(7),
                Value::Symbol("entry".to_owned()),
                Value::Float(1.5),
            ],
        )
        .expect("insert first scalar row");
    program
        .insert_row(
            "InputA",
            [
                Value::Number(11),
                Value::Symbol("next".to_owned()),
                Value::Float(-0.0),
            ],
        )
        .expect("insert second scalar row");
    program
        .insert_row("InputB", [Value::Unsigned(9)])
        .expect("insert unsigned row");
    program
        .insert_row("InputSmall", [Value::typed("Small", Value::Number(3))])
        .expect("insert subtype-small row");
    program
        .insert_row("InputLarge", [Value::typed("Large", Value::Number(101))])
        .expect("insert subtype-large row");
    program
        .insert_row("Trigger", [])
        .expect("insert nullary trigger");

    program
        .run_with_options(RunOptions::new(std::num::NonZeroUsize::new(4).unwrap()))
        .expect("run compiled process");

    assert_eq!(
        fs::read_to_string(work_dir.join("facts/InputA.facts")).expect("read scalar facts"),
        "7\tentry\t1.5\n11\tnext\t-0.0\n"
    );
    assert_eq!(
        fs::read_to_string(work_dir.join("facts/Trigger.facts")).expect("read nullary facts"),
        "\n"
    );
    assert_eq!(program.last_run_options().unwrap().threads().get(), 4);

    assert_eq!(
        program
            .read_relation("OutputA")
            .expect("read scalar output")
            .rows(),
        &[
            Row::new(vec![
                Value::Number(7),
                Value::Symbol("entry".to_owned()),
                Value::Unsigned(9),
                Value::Float(1.5),
            ]),
            Row::new(vec![
                Value::Number(11),
                Value::Symbol("next".to_owned()),
                Value::Unsigned(9),
                Value::Float(-0.0),
            ]),
        ]
    );
    let output_rows = program
        .read_relation("OutputA")
        .expect("read scalar output again");
    assert_eq!(output_rows.rows()[1].values()[3], Value::Float(-0.0));
    assert_eq!(
        output_rows.rows()[1].values()[3].clone_float_bits(),
        0x8000_0000_0000_0000
    );

    assert_eq!(
        program
            .read_relation("BucketOut")
            .expect("read union output")
            .rows(),
        &[
            Row::new(vec![Value::typed(
                "Bucket",
                Value::typed("Small", Value::Number(3)),
            )]),
            Row::new(vec![Value::typed(
                "Bucket",
                Value::typed("Small", Value::Number(101)),
            )]),
        ]
    );
    assert_eq!(
        program
            .read_relation("Fired")
            .expect("read nullary output")
            .rows(),
        &[Row::new(Vec::new())]
    );
}

#[test]
#[ignore = "requires a local Souffle install to compile a process executable"]
fn runs_composite_program_through_process_backend() {
    let Some(souffle_bin) = find_souffle_bin() else {
        eprintln!("skipping process smoke: set SOUFFLE_RS_SOUFFLE_BIN or put souffle on PATH");
        return;
    };

    let tempdir = tempfile::tempdir().expect("create tempdir");
    let logic_path = tempdir.path().join("analysis.dl");
    fs::write(
        &logic_path,
        "\
.type Pair = [id:number, label:symbol]
.type List = [head:number, tail:List]
.type Choice = Some {payload:Pair, values:List} | Empty {}
.decl ComplexIn(payload:Pair, values:List, choice:Choice)
.input ComplexIn
.decl ComplexOut(payload:Pair, values:List, choice:Choice)
.output ComplexOut
ComplexOut(payload, values, choice) :- ComplexIn(payload, values, choice).
",
    )
    .expect("write Souffle program");

    let executable = tempdir.path().join("analysis");
    compile_souffle(&souffle_bin, &logic_path, &executable);

    let work_dir = tempdir.path().join("work");
    let mut program = ProcessProgram::builder("analysis")
        .schema(composite_process_schema())
        .process_config(ProcessConfig::new(&executable, &work_dir))
        .build_process()
        .expect("build process facade");

    let payload = Value::Record(vec![Value::Number(7), Value::Symbol("entry".to_owned())]);
    let values = Value::List(vec![Value::Number(1), Value::Number(2)]);
    let choice = Value::Adt {
        variant: "Some".to_owned(),
        fields: vec![payload.clone(), values.clone()],
    };
    program
        .insert_row(
            "ComplexIn",
            [payload.clone(), values.clone(), choice.clone()],
        )
        .expect("insert composite row");
    program
        .insert_row(
            "ComplexIn",
            [
                Value::Record(vec![Value::Number(9), Value::Symbol("empty".to_owned())]),
                Value::List(Vec::new()),
                Value::Adt {
                    variant: "Empty".to_owned(),
                    fields: Vec::new(),
                },
            ],
        )
        .expect("insert empty composite row");

    program
        .run_with_options(RunOptions::default())
        .expect("run compiled process");

    assert_eq!(
        fs::read_to_string(work_dir.join("facts/ComplexIn.facts")).expect("read composite facts"),
        "[7, \"entry\"]\t[1, [2, nil]]\t$Some([7, \"entry\"], [1, [2, nil]])\n\
[9, \"empty\"]\tnil\t$Empty\n"
    );

    let output = program
        .read_relation("ComplexOut")
        .expect("read composite output");
    assert_eq!(output.rows().len(), 2);
    assert!(
        output
            .rows()
            .contains(&Row::new(vec![payload, values, choice,]))
    );
    assert!(output.rows().contains(&Row::new(vec![
        Value::Record(vec![Value::Number(9), Value::Symbol("empty".to_owned())]),
        Value::List(Vec::new()),
        Value::Adt {
            variant: "Empty".to_owned(),
            fields: Vec::new(),
        },
    ])));
}

fn compile_souffle(souffle_bin: &Path, logic_path: &Path, executable: &Path) {
    let output = Command::new(souffle_bin)
        .arg("-o")
        .arg(executable)
        .arg(logic_path)
        .output()
        .expect("spawn souffle");
    assert!(
        output.status.success(),
        "souffle compile failed with status {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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

fn process_parity_schema() -> RelationBundle {
    [
        RelationSchema::input(
            RelationId::new(0),
            "InputA",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new("label", TypeRef::Symbol),
                AttributeSchema::new("weight", TypeRef::Float),
            ],
        ),
        RelationSchema::input(
            RelationId::new(1),
            "InputB",
            [AttributeSchema::new("value", TypeRef::Unsigned)],
        ),
        RelationSchema::input(
            RelationId::new(2),
            "InputSmall",
            [AttributeSchema::new(
                "value",
                TypeRef::Subtype {
                    name: "Small".to_owned(),
                    base: Box::new(TypeRef::Number),
                },
            )],
        ),
        RelationSchema::input(
            RelationId::new(3),
            "InputLarge",
            [AttributeSchema::new(
                "value",
                TypeRef::Subtype {
                    name: "Large".to_owned(),
                    base: Box::new(TypeRef::Number),
                },
            )],
        ),
        RelationSchema::input(RelationId::new(4), "Trigger", []),
        RelationSchema::output(
            RelationId::new(5),
            "OutputA",
            [
                AttributeSchema::new("id", TypeRef::Number),
                AttributeSchema::new("label", TypeRef::Symbol),
                AttributeSchema::new("value", TypeRef::Unsigned),
                AttributeSchema::new("weight", TypeRef::Float),
            ],
        ),
        RelationSchema::output(
            RelationId::new(6),
            "BucketOut",
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
        RelationSchema::output(RelationId::new(7), "Fired", []),
    ]
    .into_iter()
    .collect()
}

fn composite_process_schema() -> RelationBundle {
    let pair = TypeRef::Record(vec![TypeRef::Number, TypeRef::Symbol]);
    let list = TypeRef::List(Box::new(TypeRef::Number));
    let choice = TypeRef::adt(
        "Choice",
        [
            ("Some".to_owned(), vec![pair.clone(), list.clone()]),
            ("Empty".to_owned(), Vec::new()),
        ],
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

trait FloatBits {
    fn clone_float_bits(&self) -> u64;
}

impl FloatBits for Value {
    fn clone_float_bits(&self) -> u64 {
        match self {
            Value::Float(value) => value.to_bits(),
            value => panic!("expected float value, got {value:?}"),
        }
    }
}

fn find_souffle_bin() -> Option<PathBuf> {
    env_path("SOUFFLE_RS_SOUFFLE_BIN")
        .or_else(|| env_path("SOUFFLE"))
        .or_else(|| find_on_path("souffle"))
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

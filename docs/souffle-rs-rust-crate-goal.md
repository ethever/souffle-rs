# `souffle-rs` Rust Crate 目标

## 背景

Souffle Datalog 的常规使用方式是：

```text
.dl
  -> souffle -o compiled_binary
  -> 外部进程执行 compiled_binary
  -> 通过 fact/output 文件目录交换 relations
```

这个方式简单、稳定、隔离性强，但在大批量合约分析时会放大小文件问题。每个合约都可能
产生一批 fact files、output relation files、stderr、metadata 和临时目录。即使单个合约
计算很快，海量小文件的 metadata、目录创建、清理、page cache 抖动和文件系统差异也会
变成吞吐瓶颈。

Souffle 还有另一条能力：把 Datalog 生成 C++，并通过 C++ interface 在程序内操作
relations：

```text
.dl
  -> souffle -g / -G generated C++
  -> C++ program creates SouffleProgram
  -> getRelation(name)
  -> insert input tuples
  -> run()
  -> iterate output tuples
```

本目标是做一个 Rust crate，把这条 embedded relation API 以 Rust-first 方式引入 Rust
生态，使 Rust 程序能够不通过 fact/output 文件目录，直接喂 Souffle input relations、
运行 Datalog、读取 output relations。

该 crate 的目标是成为通用、完整、Rust-first 的 Souffle embedded 封装：可以嵌入任意
Souffle Datalog program，不把任何下游项目的 relation、schema、client 或执行阶段写死
在 runtime 层。Gigahorse 只是一个重要的真实压力测试对象，不是 crate 的边界。

## 总目标

实现一个 Rust crate 族，暂名：

```text
souffle-rs
souffle-rs-sys
souffle-rs-build
```

目标能力：

- build time 调用 Souffle 生成 C++。
- 编译 Souffle generated C++ 和 C++ wrapper。
- Rust 通过稳定 C ABI 调用 wrapper。
- Rust safe API 支持插入 input relation rows。
- Rust safe API 支持运行 program。
- Rust safe API 支持读取 output relation rows。
- 第一版不产生 fact/output relation 文件。
- 第一版不依赖 Python。
- 第一版默认动态链接复杂外部库。
- 后续逐步支持静态链接 generated C++、wrapper、`souffle-addon` 和其他依赖。
- 最终完整覆盖 Souffle embedded relation API、Souffle 类型系统、schema introspection、
  多 program 构建、多 backend 链接和下游自定义 functor。

这个 crate 不应该让 Rust 直接依赖 Souffle C++ 类型。Rust 只能依赖我们自己定义的
C ABI。C++ wrapper 内部可以使用 Souffle C++ interface。

## 非目标

本目标明确不包含：

- 不重写 Souffle。
- 不把 Datalog 翻译成 Rust。
- 不重写任何下游项目的 Datalog core。
- 不要求 MVP 一次性支持所有 Souffle 类型和所有 IO directive；完整覆盖是最终完成条件。
- 不要求 MVP 一开始达到 crates.io 级别的长期稳定 API；长期目标是通用稳定封装。
- 不要求 MVP 一开始提供完全静态 single binary；完整静态链接是后续 release profile。
- 不让 Rust 直接绑定 `souffle::SouffleProgram`、`souffle::Relation`、`souffle::tuple`
  等 C++ 类型。
- 不跨 FFI 边界抛 C++ exception。
- 不把 embedded backend 作为唯一执行方式；process backend 仍然保留用于 parity、
  timeout 和 crash isolation。

## Crate 分层

### `souffle-rs`

Safe Rust API。

职责：

- 定义 `Program`、`ProgramConfig`、`RunOptions`。
- 定义 `RelationSchema`、`RelationId`、`RelationBundle`。
- 定义 `Value`、`Row`、`Rows`。
- 提供 `insert_row`、`run`、`relation` 等安全接口。
- 把 FFI 错误转成 Rust error。
- 管理 C side handles 的生命周期。

示例 API：

```rust
let mut program = EmbeddedProgram::new("toy")?;

program.insert_row(
    "Input",
    [Value::Number(1), Value::Symbol("hello".into())],
)?;

program.run(RunOptions { threads: 1 })?;

let rows = program.relation("Output")?;
```

### `souffle-rs-sys`

Raw FFI 层。

职责：

- 包含或生成 C ABI binding。
- 暴露 `unsafe extern "C"` 函数。
- 不包含业务逻辑。
- 不做复杂内存管理策略，只如实映射 C ABI。

该 crate 只绑定我们自己的 C header，例如：

```c
typedef struct SrProgram SrProgram;
typedef struct SrError SrError;

int sr_program_new(const char* name, SrProgram** out, SrError* err);
int sr_program_insert_row(SrProgram* program, const SrRow* row, SrError* err);
int sr_program_run(SrProgram* program, const SrRunOptions* options, SrError* err);
int sr_program_read_relation(SrProgram* program, const char* relation, SrRelationOut* out, SrError* err);
void sr_program_free(SrProgram* program);
void sr_relation_out_free(SrRelationOut* out);
```

不要绑定 Souffle 原生 C++ API。

### `souffle-rs-build`

Build helper。

职责：

- 在 `build.rs` 中调用 Souffle。
- 生成 C++ source。
- 编译 generated C++。
- 编译 C++ wrapper。
- 配置 include path。
- 配置 library path。
- 配置动态或静态链接参数。
- 生成 Rust 可用的 build metadata。

示例 API：

```rust
souffle_rs_build::Build::new()
    .program("toy", "datalog/toy.dl")
    .souffle_bin("souffle")
    .souffle_include("/home/ethever/.local/include")
    .generated_namespace("toy")
    .macro_def("PROJECT_DIR", Some("/path/to/project"))
    .library_dir("/path/to/souffle-addon")
    .link_dynamic("functors")
    .compile();
```

## 编译流程

### 输入

build helper 接收：

- program name。
- `.dl` entrypoint。
- Souffle binary path。
- Souffle include path。
- macro definitions。
- include dirs。
- library dirs。
- generated namespace。
- wrapper source path。
- link mode。

### 生成

第一版优先使用 `souffle -G`：

```text
souffle \
  -G target/souffle-rs/generated/<program> \
  -N <namespace> \
  -M <macros> \
  -L <library_dir> \
  <entrypoint.dl>
```

优先 `-G` 而不是 `-g`，因为大型 Datalog program 生成单个 C++ 文件会很大，编译慢且
不利于增量构建。

### 编译

使用 `cc` crate 或 CMake 编译：

```rust
cc::Build::new()
    .cpp(true)
    .std("c++17")
    .define("__EMBEDDED_SOUFFLE__", None)
    .include(souffle_include)
    .include(generated_dir)
    .file("wrapper.cpp")
    .files(generated_cpp_files)
    .compile("souffle_rs_<program>");
```

必须确保：

- generated C++ 和 wrapper 使用同一 C++ standard。
- generated C++ 和 wrapper 使用同一 Souffle headers。
- `__EMBEDDED_SOUFFLE__` 已定义，避免 generated C++ 自带 `main()`。
- OpenMP flags 由 build helper 明确控制。
- linker 参数由 feature 和 build config 明确控制。

### 链接

第一版默认动态链接外部复杂库：

```rust
println!("cargo:rustc-link-lib=dylib=stdc++");
println!("cargo:rustc-link-lib=dylib=gomp");
println!("cargo:rustc-link-lib=dylib=z3");
println!("cargo:rustc-link-lib=dylib=functors");
```

按平台可能是：

- Linux + GCC：`stdc++`、`gomp`。
- Linux + Clang：`c++` / `c++abi`、`omp`。
- macOS：`c++`、`omp`。

运行时可通过：

- `LD_LIBRARY_PATH`
- `DYLD_LIBRARY_PATH`
- rpath
- install name
- container / Nix / script wrapper

定位 `.so` / `.dylib`。

## C ABI 设计

### 原则

C ABI 必须保守：

- 只使用 C-compatible POD 类型。
- 不跨 ABI 传 C++ STL。
- 不跨 ABI 传 Rust-owned allocation 给 C++ 长期持有。
- 不跨 ABI 抛 C++ exception。
- 所有 C++ exception 在 wrapper 内 catch，转成 `SrError`。
- 所有内存释放由创建方提供对应 free 函数。
- ABI version 明确记录。

### 基础类型

建议：

```c
typedef enum SrValueKind {
    SR_VALUE_NUMBER,
    SR_VALUE_UNSIGNED,
    SR_VALUE_FLOAT,
    SR_VALUE_SYMBOL,
    SR_VALUE_RECORD,
    SR_VALUE_ADT,
    SR_VALUE_NULLARY
} SrValueKind;

typedef struct SrValue {
    SrValueKind kind;
    union {
        int64_t number;
        uint64_t unsigned_value;
        double float_value;
        const char* symbol;
        uint64_t record_id;
        uint64_t adt_id;
    } as;
} SrValue;

typedef struct SrRow {
    const char* relation;
    const SrValue* values;
    size_t len;
} SrRow;
```

第一版可以先不暴露 `record_id` 和 `adt_id`，只支持：

- `number`
- `symbol`
- nullary relation

### Program lifecycle

建议：

```c
int sr_program_new(const char* program_name, SrProgram** out, SrError* err);
int sr_program_set_threads(SrProgram* program, size_t threads, SrError* err);
int sr_program_insert_row(SrProgram* program, const SrRow* row, SrError* err);
int sr_program_run(SrProgram* program, SrError* err);
int sr_program_read_relation(SrProgram* program, const char* relation, SrRelationOut* out, SrError* err);
void sr_program_free(SrProgram* program);
```

### Output transfer

第一版可以用 pull model：

```c
int sr_program_read_relation(
    SrProgram* program,
    const char* relation,
    SrRelationOut* out,
    SrError* err
);
```

`SrRelationOut` 包含连续 rows。Rust 调用 `sr_relation_out_free` 释放。

后续可以支持 callback model：

```c
typedef int (*SrRowCallback)(const SrRow* row, void* user_data);

int sr_program_for_each_row(
    SrProgram* program,
    const char* relation,
    SrRowCallback callback,
    void* user_data,
    SrError* err
);
```

callback model 可以减少一次性大 relation 的内存压力，但 ABI 和错误处理更复杂。MVP
先用 pull model。

## Rust 类型模型

Safe API 中建议：

```rust
pub enum Value {
    Number(i64),
    Unsigned(u64),
    Float(f64),
    Symbol(String),
    Record(Vec<Value>),
    Adt { variant: String, fields: Vec<Value> },
    Nullary,
}

pub struct Row {
    pub values: Vec<Value>,
}

pub struct Relation {
    pub name: String,
    pub schema: Option<RelationSchema>,
    pub rows: Vec<Row>,
}

pub struct RelationBundle {
    pub relations: BTreeMap<String, Relation>,
}
```

第一版：

- `Value::Number`
- `Value::Symbol`
- nullary relation

第二版：

- `Value::Unsigned`
- `Value::Float`

第三版：

- `Value::Record`
- `Value::Adt`
- nested list/record。

## Souffle 类型支持计划

最终目标是完整覆盖 Souffle generated C++ relation API 可暴露的值类型。阶段性实现可以先
支持子集，但每个未支持类型都必须是显式 feature gap，不能被 silent fallback 成
`symbol`、`number` 或 opaque value。

完整封装至少包括：

- all primitive scalar types：`number`、`unsigned`、`float`、`symbol`。
- nullary relation。
- record。
- nested record / list。
- algebraic data type。
- subtype / union type 的 schema-visible 表示。
- relation schema introspection。
- input relation insertion。
- output relation iteration。
- generated program lifecycle。
- explicit thread control。
- custom functor link configuration。
- multiple generated programs in one Rust workspace。

只要上述任一类仍未支持，crate 可以发布 MVP 或 experimental 版本，但不能声称 complete。

### `number`

映射：

```text
Souffle RamDomain -> i64
```

注意：

- Souffle word size 可能是 64-bit。
- 需要检查 `RAM_DOMAIN_SIZE`。
- Rust 侧要避免无声截断。

### `unsigned`

映射：

```text
Souffle RamUnsigned -> u64
```

注意：

- C ABI union 中要区分 signed 和 unsigned。
- schema 必须告诉 parser 当前列类型。

### `float`

映射：

```text
Souffle RamFloat -> f64
```

注意：

- NaN、Inf、序列化 parity 需要测试。

### `symbol`

Souffle 内部把 symbol 编成 symbol table id。C++ wrapper 负责：

- Rust -> C string -> Souffle symbol table encode。
- Souffle symbol id -> decode -> C string -> Rust `String`。

Rust 不应持有 Souffle 内部 symbol id。

### record / list

Souffle record/list 通过 record table 表示。C++ wrapper 负责：

- pack Rust nested `Value` 到 Souffle record table。
- unpack Souffle record id 到 Rust nested `Value`。

这部分是高风险，不进 MVP。

### ADT

ADT 往往也依赖 record / tag 编码。需要：

- 读取 generated program 的类型 metadata。
- 或要求用户提供 schema。
- 或在 build helper 阶段从 Souffle AST / generated metadata 中抽取 schema。

这部分放在 record/list 之后。

## Schema 策略

不要让 runtime 猜 relation schema。必须有 schema 来源。

可选方案：

1. 用户在 Rust 中手写 schema。
2. build helper 从 `.dl` 中提取 `.decl`。
3. build helper 从 generated C++ relation wrapper metadata 提取 attr types / names。
4. C++ wrapper 运行时通过 `Relation::getAttrType`、`getAttrName` 暴露 schema。

推荐 MVP：

- C++ wrapper 暴露 relation 的 attr type 和 attr name。
- Rust safe API 读取 schema。
- 对 insert 操作做 arity/type check。

后续：

- build helper 生成 typed Rust structs。

例如：

```rust
#[derive(SouffleRelation)]
struct PushValue {
    stmt: Symbol,
    value: Symbol,
}
```

这不是 MVP。

## Thread / OpenMP 策略

Souffle generated C++ 可能使用 OpenMP。

crate 必须提供：

```rust
pub struct RunOptions {
    pub threads: usize,
}
```

C++ wrapper 内部调用：

```cpp
prog->setNumThreads(threads);
prog->run();
```

默认值：

- MVP 默认 `threads = 1`。
- 不使用 OpenMP auto。

原因：

- Rust batch runner 可能已经有 worker pool。
- 如果每个 worker 内部再使用多个 OpenMP threads，会产生线程爆炸。
- 调度策略应由上层显式决定。

建议后续增加：

```rust
pub struct CpuBudget {
    pub rust_workers: usize,
    pub souffle_threads_per_program: usize,
}
```

## 错误处理

错误必须 typed：

```rust
pub enum SouffleError {
    ProgramNotFound,
    RelationNotFound { relation: String },
    ArityMismatch { relation: String, expected: usize, actual: usize },
    TypeMismatch { relation: String, column: usize, expected: String, actual: String },
    RunFailed { message: String },
    CxxException { message: String },
    AbiError { code: i32, message: String },
    BuildError { message: String },
}
```

C++ wrapper 必须 catch：

```cpp
try {
    ...
} catch (const std::exception& e) {
    sr_error_set(error, e.what());
    return SR_ERR_EXCEPTION;
} catch (...) {
    sr_error_set(error, "unknown C++ exception");
    return SR_ERR_EXCEPTION;
}
```

不要让 C++ exception 穿过 Rust FFI。

## 内存管理

原则：

- Rust 传入 input rows，C++ wrapper 立即复制到 Souffle relation。
- C++ 不持有 Rust input memory。
- C++ 返回 output buffer，由 C++ 分配，由 C++ free 函数释放。
- Rust safe API 把 output buffer copy 成 Rust-owned values 后立即释放 C++ buffer。

MVP 不追求零拷贝。先追求 ABI 简单和安全。

后续可以优化：

- callback streaming output。
- Rust arena。
- relation chunk iterator。

## Link 模式

### `dynamic` 默认

默认 feature：

```toml
[features]
default = ["dynamic"]
dynamic = []
```

含义：

- generated C++ 和 wrapper 编进 Rust artifact。
- `libstdc++` / `libgomp` / `libz3` / `libfunctors` 动态链接。

优点：

- 最容易跑通。
- 最接近系统包管理方式。
- 不阻塞 embedded relation API。

### `static-generated`

含义：

- generated C++ 和 wrapper 静态编进 Rust binary。
- 外部复杂库仍动态链接。

实际上这是 `cc` crate 默认会做到的基础能力。

### `static-addon`

含义：

- `souffle-addon` 产出 `libfunctors.a`。
- crate 静态链接 `libfunctors.a`。
- Z3/OpenMP/C++ runtime 可继续动态链接。

需要：

- 修改 `souffle-addon/Makefile`。
- 保证 object 编译参数一致。
- 保证 `RAM_DOMAIN_SIZE` 一致。

### `fully-static`

实验 feature：

- 尝试静态链接 Z3。
- 尝试静态链接 OpenMP runtime。
- 尝试静态链接 C++ standard library。
- 尝试 musl 或其他可控 toolchain。

不作为 MVP。它是发布目标，不是架构前置条件。

## Build Metadata

每次 build 应生成 metadata：

```json
{
  "program": "main",
  "souffle_version": "2.4.1",
  "souffle_bin": "/home/ethever/.local/bin/souffle",
  "entrypoint": "logic/main.dl",
  "macros": {
    "PROJECT_DIR": "..."
  },
  "generated_namespace": "analysis_main",
  "link_mode": "dynamic",
  "openmp": true,
  "sqlite": true,
  "zlib": true
}
```

Rust runtime 可暴露：

```rust
EmbeddedProgram::build_info()
```

用于 diagnostics 和 reproducibility。

## MVP

最小可用版本只做 toy program。

### Datalog

```souffle
.decl Input(x:number, s:symbol)
.input Input(IO="file", filename="Input.facts")

.decl Output(x:number, s:symbol)
.output Output(IO="file", filename="Output.csv")

Output(x, s) :- Input(x, s).
```

注意：即使 `.input/.output` 写 `IO="file"`，embedded backend 只调用 `run()`，不调用
`loadAll()` / `printAll()`，所以不会读写文件。

### Rust test

```rust
#[test]
fn embedded_toy_roundtrip() {
    let mut p = EmbeddedProgram::new("toy").unwrap();
    p.insert_row("Input", [Value::Number(7), Value::Symbol("seven".into())]).unwrap();
    p.run(RunOptions { threads: 1 }).unwrap();
    let out = p.relation("Output").unwrap();
    assert_eq!(out.rows.len(), 1);
}
```

验收：

- `cargo test` 通过。
- 不产生 `Input.facts`。
- 不产生 `Output.csv`。
- `strace -f -e openat` 可证明没有 fact/output 文件 IO。
- generated C++ 和 wrapper 被编入 test binary。
- 外部复杂库可动态链接。

## 大型 Souffle 程序接入前置目标

在接入 Gigahorse 这类大型真实 Souffle 程序前，crate 必须完成：

- 支持 `number`。
- 支持 `symbol`。
- 支持 nullary relation。
- 支持读取 relation schema。
- 支持多个 input relation。
- 支持多个 output relation。
- 支持显式线程数。
- 支持动态链接 `souffle-addon/libfunctors.so`。
- 支持 build-time macro。
- 支持 `-L souffle-addon`。
- 支持 generated namespace。
- 支持 relation name lookup。
- 支持 missing relation diagnostics。

然后再接真实大型程序，例如：

- `logic/main.dl`。
- `logic/fallback_scalable.dl`。
- `logic/last_resort.dl`。
- `clientlib/function_inliner.dl`。
- project-specific Datalog clients。

## 大型 Souffle 程序接入风险

### Relation artifact 名不一致

某些下游 pipeline 的 file backend 会通过文件名连接 stages。Gigahorse 是典型例子：

```text
producer relation: IRPublicFunction
file artifact:     PublicFunction.csv
consumer relation: PublicFunctionSelector
```

embedded backend 中没有文件名作为中间层，因此必须显式建 artifact mapping：

```rust
pub struct RelationArtifactMap {
    pub producer_relation: String,
    pub artifact_name: String,
    pub consumer_relation: String,
}
```

否则 main output 无法正确喂给 clientlib input。

### Records / list 类型

大型 Souffle 程序可能使用 list / record / ADT 相关类型或自定义 functor。MVP 可以不读写
这些 relation，但完整封装必须实现 record table pack/unpack，并能按 schema 正确暴露
这些值。

### Timeout 和 crash isolation

embedded backend 不能像 process backend 一样简单 kill 子进程。

建议：

- process backend 保留为 fallback。
- embedded backend 初期用于 batch 中的正常 case。
- long-running 或不可信 case 可以回退 process backend。
- 后续探索 watchdog process 或 worker process pool。

### OpenMP 与 Rust worker 嵌套

默认 `threads = 1`。上层 batch runner 明确决定：

```text
rust_workers * souffle_threads_per_program <= cpu_budget
```

## Benchmark 目标

crate 成功后必须证明它解决小文件问题，而不是只改变语言边界。

Benchmark：

- toy program：file backend vs embedded backend。
- small downstream fixture：process backend vs embedded backend。
- batch 100 contracts。
- batch 10,000 contracts。

指标：

- total time。
- core run time。
- fact materialization time。
- relation read time。
- files created。
- bytes written。
- peak RSS。
- CPU utilization。
- OpenMP threads。

成功标准：

- embedded backend fact/output 文件数为 0。
- toy benchmark 无 fact/output file IO。
- 真实大型程序 fixture 输出与 process backend parity。
- 大批量场景小文件相关时间显著下降。

## 分阶段计划

### 阶段 1：Toy proof

目标：

- build.rs 调 `souffle -G`。
- 编译 generated C++。
- 编译 C++ wrapper。
- Rust 插入 input relation。
- Rust 运行 program。
- Rust 读取 output relation。

验收：

- toy test 通过。
- 无 fact/output 文件。
- 不调用 Python。

### 阶段 2：Crate 边界

目标：

- 拆出 `souffle-rs`、`souffle-rs-sys`、`souffle-rs-build`。
- 定义 API。
- 定义 C ABI。
- 定义 build helper。

验收：

- toy program 可由外部 crate 使用。
- 文档说明动态链接要求。

### 阶段 3：Schema 和类型检查

目标：

- 读取 relation schema。
- insert 时做 arity/type check。
- output 解码为 Rust `Value`。

验收：

- 错误 relation name 有 typed error。
- arity mismatch 有 typed error。
- type mismatch 有 typed error。

### 阶段 4：动态链接稳定化

目标：

- Linux GCC 动态链接跑通。
- rpath / env 配置清晰。
- build metadata 输出。

验收：

- 干净环境能按 README 跑 toy test。
- `ldd` 输出符合预期。

### 阶段 5：自定义 functor 动态链接

目标：

- 支持下游自定义 functor 动态库，例如 `souffle-addon/libfunctors.so`。
- 支持 `-L souffle-addon`。
- 支持下游 macro，例如 `PROJECT_DIR`。

验收：

- 能生成并编译使用 functor 的 small `.dl`。
- 不通过文件 IO 喂 input relation。

### 阶段 6：真实大型程序 MVP

目标：

- 能嵌入真实大型 Souffle entrypoint，例如 `logic/main.dl`。
- 能插入最小 facts。
- 能读取少量 output relation。

验收：

- 与 process backend 对同一 fixture 的目标 relation 输出一致。

### 阶段 7：静态 addon

目标：

- `souffle-addon` 产出 `libfunctors.a`。
- `souffle-rs-build` 支持 `static-addon`。

验收：

- `ldd` 不再显示 `libfunctors.so`。
- Z3/OpenMP/C++ runtime 可继续动态链接。

### 阶段 8：Record / ADT 支持

目标：

- 支持 record pack/unpack。
- 支持 list。
- 支持 ADT。

验收：

- 使用 record/list 的 Souffle fixture roundtrip。
- 真实大型程序需要暴露的 record/list relation 可读写。

### 阶段 9：Fully static experiment

目标：

- 尝试静态 Z3。
- 尝试静态 OpenMP。
- 尝试静态 C++ runtime。

验收：

- 作为独立 release profile。
- 不阻塞默认 dynamic profile。

## 完成定义

本 goal 完成时，应能回答：

- Rust 如何在不写 fact/output 文件的情况下运行 Souffle Datalog？
- build.rs 如何生成和编译 Souffle C++？
- Rust 和 C++ 之间的 ABI 边界是什么？
- 支持哪些 Souffle 类型？
- 如何新增一个 Datalog program？
- 如何设置 OpenMP 线程数？
- 如何动态链接外部依赖？
- 如何逐步静态链接 generated C++、wrapper、addon 和其他库？
- 如何证明没有 Python？
- 如何证明没有 fact/output 小文件？
- 如何和 process backend 做 parity？

最终完成条件：

> `souffle-rs` 提供一个 Rust-first embedded Souffle runtime：Rust 程序可以通过
> typed relation API 插入 facts、运行 Datalog、读取 output relations；第一版默认动态
> 链接复杂外部库，但 generated C++ 和 wrapper 直接编入 Rust 构建；后续可以
> 分级推进静态链接。该 crate 的完成标准是完整、通用地封装 Souffle embedded 能力，并
> 让任意下游 Datalog 程序都能选择无 fact/output 小文件的 Rust embedded 执行路径。

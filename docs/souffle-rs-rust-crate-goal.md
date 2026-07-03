# `souffle-rs` Rust Crate 最终目标

## 定位

`souffle-rs` 是一个通用、完整、Rust-first 的 Souffle embedded runtime 与 build
tooling。它的目标不是包装某一个下游项目，也不是只把某几个 relation 跑通，而是让 Rust
程序能够完整嵌入任意 Souffle Datalog program，并用 Rust API 直接控制：

- Datalog program 的生成、编译、链接和加载。
- input relation 的写入。
- program 的运行。
- output relation 的读取、流式迭代和导出。
- schema、类型、错误、线程、链接和 build metadata。

`souffle-rs` 必须把 Souffle 的能力完整带入 Rust 生态。Gigahorse 只是一个真实压力测试
对象，不能定义 crate 的边界；runtime 层不得写死任何 Gigahorse relation、schema、
client、artifact name 或 pipeline stage。

## 核心问题

Souffle 的常规工程使用方式通常是：

```text
.dl
  -> souffle -o compiled_binary
  -> 外部进程执行 compiled_binary
  -> 通过 fact/output 文件目录交换 relations
```

这个模型隔离性强，但在大批量程序分析中会放大小文件问题。每个分析对象都可能产生多组
fact files、output relation files、stderr、metadata 和临时目录。海量小文件会让目录
metadata、page cache、fsync、清理、路径分配和文件系统差异成为吞吐瓶颈。

Souffle 同时提供 generated C++ 与 embedded C++ interface：

```text
.dl
  -> souffle -g / -G generated C++
  -> C++ program creates SouffleProgram
  -> getRelation(name)
  -> insert input tuples
  -> run()
  -> iterate output tuples
```

`souffle-rs` 要把这条 embedded relation path 变成 Rust 的一等能力。完成后的 Rust 用户
不需要 Python 调度层，不需要为 relation exchange 创建 fact/output 小文件，也不需要在
Rust 中直接触碰 Souffle C++ 类型。

## 总完成要求

`souffle-rs` 完成时必须满足以下要求：

- 完整嵌入任意 Souffle Datalog program。
- 完整支持 Souffle generated C++ relation API 能表达的 relation schema 和 value。
- 完整支持 `number`、`unsigned`、`float`、`symbol`、nullary relation、record、nested
  record/list、ADT、subtype / union type 的 schema-visible 表示。
- 完整支持 input relation insertion、output relation iteration、relation schema
  introspection、program lifecycle 和 explicit thread control。
- 完整支持动态 relation API 与 generated typed Rust API。
- 完整支持多个 generated program 同时存在于同一个 Rust workspace。
- 完整支持 Souffle include path、macro definition、component、generated namespace、
  custom functor、addon library 和外部依赖链接。
- 完整支持 generated C++、C++ wrapper、custom functor library、OpenMP runtime、C++
  runtime、Z3 等依赖的动态链接和静态链接配置。
- 完整支持 in-process embedded backend 与 isolated process backend，并保证二者可用同一
  Rust facade 做 parity。
- embedded relation exchange 不产生 fact/output 小文件。
- Python 不参与 build、runtime、test、benchmark 或下游集成路径。
- 所有错误必须 typed、可诊断、可定位 relation / column / type / backend / linker /
  generated artifact。
- 所有 FFI 边界必须内存安全、异常安全、线程语义明确。
- 所有 public API 必须是通用 Souffle API，不允许把任何下游项目的字段、relation、
  contract、client 或 pipeline 名字写进 runtime 层。

如果上述任一项未完成，goal 就没有完成。

## 非目标

本 crate 不做以下事情：

- 不重写 Souffle。
- 不把 Datalog 翻译成 Rust。
- 不重新实现 Souffle 的 optimizer、RAM engine、type checker 或 parser。
- 不让 Rust public API 直接依赖 `souffle::SouffleProgram`、`souffle::Relation`、
  `souffle::tuple` 等 C++ 类型。
- 不跨 FFI 边界传递 C++ STL 对象。
- 不跨 FFI 边界抛 C++ exception。
- 不让 C++ wrapper 长期持有 Rust-owned allocation。
- 不把 file backend、SQLite backend 或 process backend 删除；它们必须作为 parity、
  export、debug、timeout 和 crash isolation 的显式 backend 保留。

## Crate 分层

### `souffle-rs`

Safe Rust API。

职责：

- 定义 `Program`、`ProgramConfig`、`RunOptions`、`Backend`、`CpuBudget`。
- 定义 `RelationSchema`、`RelationId`、`RelationHandle`、`RelationBundle`。
- 定义完整 `Value`、`Row`、`Rows`、`RelationOutput`、`RelationIterator`。
- 定义 typed error：build、link、load、schema、insert、run、read、stream、FFI。
- 提供 dynamic API：按 relation name 插入和读取 rows。
- 提供 typed API：由 schema/codegen 生成 Rust structs 与 type-safe relation handles。
- 提供 schema introspection。
- 提供 streaming / chunked output iteration。
- 提供 relation export adapters：memory、file、SQLite。
- 管理 C side handles、buffers、callbacks、thread configuration 和 lifetime。

示例 API 形态：

```rust
let mut program = EmbeddedProgram::builder("analysis")
    .threads(8)
    .backend(Backend::Embedded)
    .build()?;

program.insert_row(
    "Input",
    [
        Value::Number(1),
        Value::Symbol("entry".into()),
        Value::Record(vec![Value::Unsigned(7), Value::Float(1.5)]),
    ],
)?;

program.run()?;

let schema = program.relation_schema("Output")?;
let mut rows = program.iter_relation("Output")?;

while let Some(row) = rows.next()? {
    consume(row, &schema)?;
}
```

### `souffle-rs-sys`

Raw FFI 层。

职责：

- 只绑定 `souffle-rs` 自己定义的 C ABI。
- 暴露 `unsafe extern "C"` 函数。
- 映射 C ABI 类型、错误码、opaque handles 和 free functions。
- 不包含业务逻辑。
- 不绑定 Souffle 原生 C++ API。
- 不暴露 C++ STL、C++ exception、C++ template 或 Souffle implementation detail。

### `souffle-rs-build`

Build helper。

职责：

- 在 `build.rs` 中调用 Souffle 生成 C++。
- 支持 `souffle -G` directory output 和 `souffle -g` single-file output。
- 编译 generated C++。
- 编译 C++ wrapper。
- 配置 include path、macro definition、library path、generated namespace。
- 配置 custom functor / addon library。
- 配置 OpenMP、C++ runtime、Z3、zlib、SQLite 等依赖。
- 配置动态链接、静态链接、rpath 和 install-name。
- 生成 Rust build metadata。
- 生成 schema metadata。
- 生成 typed Rust API。
- 生成 C header、Rust sys bindings 和 ABI version metadata。

示例 API 形态：

```rust
souffle_rs_build::Build::new()
    .program("analysis", "logic/main.dl")
    .souffle_bin("souffle")
    .souffle_include("/opt/souffle/include")
    .generated_namespace("analysis")
    .define("PROJECT_DIR", "/path/to/project")
    .include_dir("logic/include")
    .library_dir("souffle-addon")
    .functor_library(
        FunctorLibrary::new("functors")
            .search_path("souffle-addon")
            .link_library("z3")
            .link_library("gomp")
    )
    .link_mode(LinkMode::StaticGeneratedAndConfiguredExternal)
    .emit_schema(true)
    .emit_typed_api(true)
    .compile();
```

## Build 流程要求

### 输入

build helper 必须接收并记录：

- program name。
- `.dl` entrypoint。
- Souffle binary path。
- Souffle version。
- Souffle include path。
- include dirs。
- library dirs。
- macro definitions。
- generated namespace。
- generated output mode。
- wrapper source path。
- custom functor libraries。
- C++ standard。
- compiler path。
- OpenMP configuration。
- link mode。
- rpath / install-name configuration。
- schema/codegen output paths。

### 生成

build helper 必须支持：

```text
souffle \
  -G target/souffle-rs/generated/<program> \
  -N <namespace> \
  -M <macro>=<value> \
  -L <library_dir> \
  <entrypoint.dl>
```

也必须支持需要 single-file output 的场景：

```text
souffle \
  -g target/souffle-rs/generated/<program>.cpp \
  -N <namespace> \
  -M <macro>=<value> \
  -L <library_dir> \
  <entrypoint.dl>
```

生成路径必须稳定、可缓存、可清理、可重建。build script 必须正确输出
`cargo:rerun-if-changed` 和 `cargo:rerun-if-env-changed`，避免 stale generated C++。

### 编译

编译要求：

- generated C++ 和 wrapper 使用同一 C++ standard。
- generated C++ 和 wrapper 使用同一 Souffle headers。
- `__EMBEDDED_SOUFFLE__` 必须定义，避免 generated C++ 自带 `main()`。
- OpenMP flags 必须由 build config 明确控制。
- linker flags 必须由 link config 明确控制。
- 编译失败必须输出完整 command、working directory、include dirs、library dirs、stderr。
- build metadata 必须记录 compiler、flags、Souffle version、generated files 和 ABI version。

### 链接

link mode 必须完整覆盖：

- generated C++ 和 wrapper 编入 Rust artifact。
- custom functor library 动态链接。
- custom functor library 静态链接。
- C++ standard library 动态链接。
- C++ standard library 静态链接。
- OpenMP runtime 动态链接。
- OpenMP runtime 静态链接。
- Z3 动态链接。
- Z3 静态链接。
- zlib / SQLite 等 Souffle build 依赖的动态或静态链接。
- Linux GCC。
- Linux Clang。
- macOS Clang。
- rpath / install-name 配置。

任何 link mode 不可用时，必须给出 typed build error，说明缺失的 library、search path、
symbol、compiler flag 或 platform capability。不得静默降级。

## C ABI 设计

### 原则

C ABI 必须稳定、清晰、完整：

- 所有 public C symbol 使用 `souffle_rs_` 前缀。
- 所有 public C type 使用 `SouffleRs` 前缀。
- 所有 public C enum value 使用 `SOUFFLE_RS_` 前缀。
- 只使用 C-compatible 类型。
- 不跨 ABI 传 C++ STL。
- 不跨 ABI 传 Rust-owned allocation 给 C++ 长期持有。
- 不跨 ABI 抛 C++ exception。
- 所有 C++ exception 在 wrapper 内 catch 并转成 `SouffleRsError`。
- 所有 allocation 都必须有明确 owner 和对应 free function。
- ABI version 必须可查询。
- ABI mismatch 必须是 typed error。

### Program lifecycle

```c
typedef struct SouffleRsProgram SouffleRsProgram;
typedef struct SouffleRsError SouffleRsError;
typedef struct SouffleRsRunOptions SouffleRsRunOptions;
typedef struct SouffleRsRelationOutput SouffleRsRelationOutput;

int souffle_rs_program_new(
    const char* program_name,
    SouffleRsProgram** program_output,
    SouffleRsError* error
);

int souffle_rs_program_set_threads(
    SouffleRsProgram* program,
    size_t thread_count,
    SouffleRsError* error
);

int souffle_rs_program_insert_row(
    SouffleRsProgram* program,
    const SouffleRsRow* row,
    SouffleRsError* error
);

int souffle_rs_program_run(
    SouffleRsProgram* program,
    const SouffleRsRunOptions* options,
    SouffleRsError* error
);

int souffle_rs_program_read_relation(
    SouffleRsProgram* program,
    const char* relation_name,
    SouffleRsRelationOutput* relation_output,
    SouffleRsError* error
);

int souffle_rs_program_for_each_row(
    SouffleRsProgram* program,
    const char* relation_name,
    SouffleRsRowCallback callback,
    void* user_data,
    SouffleRsError* error
);

void souffle_rs_program_free(SouffleRsProgram* program);
void souffle_rs_relation_output_free(SouffleRsRelationOutput* relation_output);
```

### Value representation

C ABI 不得把 Souffle internal record id 或 ADT id 暴露为 Rust 用户语义。C ABI 只能在
内部传输 composite reference；safe Rust API 必须看到结构化值。

```c
typedef enum SouffleRsValueKind {
    SOUFFLE_RS_VALUE_NUMBER,
    SOUFFLE_RS_VALUE_UNSIGNED,
    SOUFFLE_RS_VALUE_FLOAT,
    SOUFFLE_RS_VALUE_SYMBOL,
    SOUFFLE_RS_VALUE_RECORD,
    SOUFFLE_RS_VALUE_LIST,
    SOUFFLE_RS_VALUE_ADT,
    SOUFFLE_RS_VALUE_NULLARY
} SouffleRsValueKind;

typedef struct SouffleRsString {
    const char* data;
    size_t len;
} SouffleRsString;

typedef struct SouffleRsCompositeRef {
    size_t index;
} SouffleRsCompositeRef;

typedef struct SouffleRsValue {
    SouffleRsValueKind kind;
    union {
        int64_t number;
        uint64_t unsigned_value;
        double float_value;
        SouffleRsString symbol;
        SouffleRsCompositeRef composite;
    } as;
} SouffleRsValue;

typedef struct SouffleRsRow {
    const char* relation_name;
    const SouffleRsValue* values;
    size_t len;
} SouffleRsRow;
```

Composite values 必须通过 C ABI 可遍历：

- record fields。
- list elements。
- ADT variant name。
- ADT fields。
- nested composite graph。

Rust safe API 必须把这些值 materialize 成：

```rust
pub enum Value {
    Number(i64),
    Unsigned(u64),
    Float(f64),
    Symbol(String),
    Record(Vec<Value>),
    List(Vec<Value>),
    Adt { variant: String, fields: Vec<Value> },
    Nullary,
}
```

## Souffle 类型系统要求

必须完整支持：

- `number`。
- `unsigned`。
- `float`。
- `symbol`。
- nullary relation。
- record。
- list。
- nested record / list。
- algebraic data type。
- subtype。
- union type。
- relation arity。
- relation attribute name。
- relation attribute type。
- relation qualifier。
- input relation。
- output relation。
- intermediate relation introspection where generated C++ exposes it。

类型处理要求：

- 不允许把未知类型静默降级成 `symbol`。
- 不允许把未知类型静默降级成 `number`。
- 不允许把 composite value 暴露成 opaque id。
- 不允许忽略 schema mismatch。
- 不允许在 integer width 不一致时静默截断。
- `float` 必须处理 NaN、Inf、signed zero 和 parity serialization。
- `symbol` 必须由 wrapper 负责 encode/decode Souffle symbol table，Rust 不持有内部 symbol id。
- record/list/ADT 必须由 wrapper 负责 pack/unpack Souffle record table。
- subtype / union type 必须在 schema 中可见，Rust API 必须能保留 declared type 信息。

## Schema 要求

runtime 不得猜 schema。schema 必须来自可靠来源：

- generated C++ relation metadata。
- Souffle relation API 暴露的 attr name / attr type。
- build-time extracted `.decl` metadata。
- build helper 生成的 schema artifact。

schema 必须包括：

- relation name。
- relation kind：input、output、intermediate、printable、loadable。
- arity。
- attribute names。
- attribute declared types。
- normalized runtime value types。
- nullary relation marker。
- record/list/ADT metadata。
- subtype / union type metadata。

insert 时必须做：

- relation existence check。
- arity check。
- declared type check。
- composite shape check。
- ADT variant check。
- subtype / union compatibility check。

read 时必须做：

- relation existence check。
- output eligibility check。
- schema-driven decode。
- composite unpack。
- UTF-8 validation for symbols。
- typed error on decode failure。

## Typed Rust API 要求

dynamic API 必须存在，但 complete goal 还要求 generated typed API。

build helper 必须能为 relation 生成 Rust 类型：

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct InputRow {
    pub id: i64,
    pub label: String,
    pub payload: InputPayload,
}
```

typed API 必须支持：

- typed input insertion。
- typed output iteration。
- typed schema access。
- compile-time arity where possible。
- generated conversion between Rust structs and dynamic `Value`。
- generated error context preserving relation and column names。
- opt-in namespace/module layout for multiple Souffle programs。

dynamic API 与 typed API 必须共享同一 runtime core，不能维护两套行为。

## IO 与 Backend 要求

`souffle-rs` 必须支持同一 program 的多种执行 backend：

- embedded in-process backend。
- isolated process backend。
- file relation backend。
- SQLite relation backend。
- in-memory relation backend。

embedded backend 的 relation exchange 必须走 memory relation API，不产生 fact/output 小文件。

file / SQLite backend 必须作为显式选择存在，用于：

- parity。
- debugging。
- interoperability。
- large artifact export。
- crash isolation。
- timeout isolation。

同一个 fixture 在 embedded backend 与 process/file backend 上运行时，指定 output relations
必须 byte-for-byte 或 schema-normalized equivalent。差异必须有 typed diagnostic。

## Custom Functor 与 Addon 要求

必须支持 Souffle custom functor 和 addon library：

- build-time `-L` library path。
- dynamic functor library。
- static functor library。
- dependent libraries，例如 Z3、OpenMP、C++ runtime。
- include dirs。
- macro definitions。
- symbol visibility。
- rpath / install-name。
- generated metadata 中记录 functor library name、path、link mode、dependent libs。

functor failure 必须能定位：

- missing library。
- missing symbol。
- incompatible C++ ABI。
- incompatible `RAM_DOMAIN_SIZE`。
- incompatible Souffle version。
- dependent library load failure。

## Thread / OpenMP 要求

线程控制必须显式：

```rust
pub struct RunOptions {
    pub threads: usize,
}
```

runtime 必须调用 generated program 的 thread control：

```cpp
program->setNumThreads(thread_count);
program->run();
```

要求：

- 不依赖 OpenMP auto 作为隐藏默认策略。
- 不让环境变量悄悄覆盖 Rust API 的线程预算。
- Rust worker pool 与 Souffle OpenMP threads 的组合必须可表达。
- oversubscription 必须可诊断。
- build metadata 必须记录 OpenMP 是否启用、runtime library 和 flags。

## Error 要求

错误必须 typed：

```rust
pub enum SouffleError {
    Build(BuildError),
    Link(LinkError),
    Abi(AbiError),
    ProgramNotFound { program: String },
    RelationNotFound { relation: String },
    SchemaUnavailable { relation: String },
    ArityMismatch { relation: String, expected: usize, actual: usize },
    TypeMismatch { relation: String, column: String, expected: String, actual: String },
    AdtVariantMismatch { relation: String, column: String, variant: String },
    RunFailed { program: String, message: String },
    CxxException { message: String },
    DecodeFailed { relation: String, column: String, message: String },
    BackendParityMismatch { relation: String, message: String },
}
```

C++ wrapper 必须 catch：

```cpp
try {
    // C++ relation operation
} catch (const std::exception& e) {
    souffle_rs_error_set(error, e.what());
    return SOUFFLE_RS_ERROR_EXCEPTION;
} catch (...) {
    souffle_rs_error_set(error, "unknown C++ exception");
    return SOUFFLE_RS_ERROR_EXCEPTION;
}
```

不得让 C++ exception 穿过 Rust FFI。

## Memory 要求

内存所有权必须明确：

- Rust 传入 input rows，C++ wrapper 立即复制到 Souffle relation。
- C++ 不长期持有 Rust input memory。
- C++ 返回 output buffer，由 C++ 分配，由 C++ free function 释放。
- Rust safe API materialize 成 Rust-owned values 后释放 C++ buffer。
- streaming callback 不得 panic 穿过 FFI。
- callback error 必须转成 C ABI error code。
- large relation 必须支持 chunked / streaming iteration。
- 所有 FFI buffer 必须有 fuzz / sanitizer / leak test。

## Build Metadata 要求

每次 build 必须生成 machine-readable metadata：

```json
{
  "program": "analysis",
  "souffle_version": "2.4.1",
  "souffle_bin": "/opt/souffle/bin/souffle",
  "entrypoint": "logic/main.dl",
  "macros": {
    "PROJECT_DIR": "/path/to/project"
  },
  "generated_namespace": "analysis",
  "generated_mode": "directory",
  "link_mode": "static-generated-and-configured-external",
  "openmp": {
    "enabled": true,
    "runtime": "gomp",
    "link_mode": "dynamic"
  },
  "libraries": [
    {
      "name": "functors",
      "kind": "custom-functor",
      "link_mode": "static"
    }
  ],
  "abi_version": "1",
  "schema_artifact": "target/souffle-rs/schema/analysis.json"
}
```

runtime 必须能暴露：

```rust
program.build_info()?;
program.abi_version()?;
program.schema_bundle()?;
```

## Parity 要求

必须建立与 Souffle CLI / process backend 的 parity suite。

测试对象必须覆盖：

- scalar values。
- nullary relations。
- records。
- nested records。
- lists。
- ADTs。
- subtype / union type。
- custom functors。
- multiple input relations。
- multiple output relations。
- large output relation streaming。
- multiple generated programs。
- file backend。
- SQLite backend。
- embedded backend。

parity 规则：

- 同一 input relation bundle。
- 同一 Datalog entrypoint。
- 同一 macro definitions。
- 同一 generated namespace。
- 同一 thread count，除非测试目标就是 thread parity。
- 输出按 schema-normalized rows 比较。
- symbol decode 后比较。
- float 按明确规则比较 NaN、Inf、signed zero。

## Performance 要求

crate 必须证明它解决小文件问题，而不是只改变语言边界。

benchmark 必须记录：

- total time。
- Souffle run time。
- relation insertion time。
- relation output decode time。
- file count。
- bytes written。
- metadata operations。
- peak RSS。
- CPU utilization。
- OpenMP thread count。
- Rust worker count。
- backend type。

成功标准：

- embedded relation exchange 的 fact/output 文件数为 0。
- embedded backend 在大批量小 relation 场景中显著减少 filesystem overhead。
- streaming output 不因单个大 relation 强制 materialize 全量内存。
- typed API 不比 dynamic API 引入不必要的额外 relation copy。

## Documentation 要求

文档必须说明：

- 如何添加一个 Souffle program。
- 如何配置 Souffle binary 和 include path。
- 如何配置 generated namespace。
- 如何配置 macro definitions。
- 如何配置 custom functor。
- 如何配置动态链接。
- 如何配置静态链接。
- 如何设置 OpenMP threads。
- 如何插入 dynamic rows。
- 如何使用 generated typed rows。
- 如何读取 output relation。
- 如何 streaming 大 relation。
- 如何导出到 file / SQLite。
- 如何做 embedded 与 process backend parity。
- 如何诊断 build/link/runtime/schema/type 错误。

## CI 与验收要求

CI 必须覆盖：

- `cargo fmt --all --check`。
- `cargo clippy --workspace --all-targets --all-features`。
- `cargo test --workspace --all-features`。
- generated C++ build tests。
- embedded runtime tests。
- process backend parity tests。
- full Souffle type parity tests。
- custom functor dynamic link tests。
- custom functor static link tests。
- Linux GCC。
- Linux Clang。
- macOS Clang。
- sanitizer or leak-check job for FFI buffers。

完成验收必须证明：

- 没有 Python runtime dependency。
- embedded relation exchange 没有 fact/output 小文件。
- Rust 不直接依赖 Souffle C++ public types。
- C ABI 名称完整、稳定、可读。
- Souffle 类型系统完整映射到 Rust。
- relation schema 可 introspect。
- dynamic API 和 typed API 行为一致。
- in-process backend 与 isolated process backend parity。
- custom functor 和 addon library 可动态链接和静态链接。
- OpenMP 线程数由 Rust API 明确控制。
- build metadata 足够复现生成、编译和链接过程。

## 最终完成定义

`souffle-rs` 完成时，Rust 用户必须能够在一个普通 Rust workspace 中引入任意 Souffle
Datalog program，通过 `build.rs` 生成并编译 Souffle C++，通过安全 Rust API 插入完整
类型系统覆盖的 input relations，运行 program，按 schema 正确读取或流式遍历 output
relations，并在 embedded backend 中完全绕开 fact/output 小文件。

同一个 crate 还必须提供 process/file/SQLite backend 做 parity、debug、export、timeout
和 crash isolation；必须支持 custom functor、OpenMP、动态链接、静态链接和 build
metadata；必须用 typed errors 暴露所有 build、link、schema、type、FFI 和 runtime
问题。

只有当上述能力全部实现、测试和文档化之后，本 goal 才算完成。

mod targets;
use targets::rustc_id_and_flags;
pub use targets::{targets, GodboltTargets};

use crate::{Context, Error};

const LLVM_MCA_TOOL_ID: &str = "llvm-mcatrunk";

enum Compilation {
    Success {
        asm: String,
        stderr: String,
        llvm_mca: Option<String>,
    },
    Error {
        stderr: String,
    },
}

#[derive(Debug, serde::Deserialize)]
struct GodboltOutputSegment {
    text: String,
}

#[derive(Debug, serde::Deserialize)]
struct GodboltOutput(Vec<GodboltOutputSegment>);

impl GodboltOutput {
    pub fn full_with_ansi_codes_stripped(&self) -> Result<String, Error> {
        let mut complete_text = String::new();
        for segment in self.0.iter() {
            complete_text.push_str(&segment.text);
            complete_text.push('\n');
        }
        Ok(String::from_utf8(strip_ansi_escapes::strip(
            complete_text.trim(),
        )?)?)
    }
}

#[derive(Debug, serde::Deserialize)]
struct GodboltResponse {
    code: u8,
    // stdout: GodboltOutput,
    stderr: GodboltOutput,
    asm: GodboltOutput,
    tools: Vec<GodboltTool>,
}

#[derive(Debug, serde::Deserialize)]
struct GodboltTool {
    id: String,
    // code: u8,
    stdout: GodboltOutput,
    // stderr: GodboltOutput,
}

/// Compile a given Rust source code file on Godbolt using the latest nightly compiler with
/// full optimizations (-O3)
/// Returns a multiline string with the pretty printed assembly
async fn compile_rust_source(
    http: &reqwest::Client,
    source_code: &str,
    rustc: &str,
    flags: &str,
    run_llvm_mca: bool,
) -> Result<Compilation, Error> {
    let tools = if run_llvm_mca {
        serde_json::json! {
            [{"id": LLVM_MCA_TOOL_ID}]
        }
    } else {
        serde_json::json! {
            []
        }
    };

    let request = http
        .post(&format!(
            "https://godbolt.org/api/compiler/{}/compile",
            rustc
        ))
        .header(reqwest::header::ACCEPT, "application/json") // to make godbolt respond in JSON
        .json(&serde_json::json! { {
            "source": source_code,
            "options": {
                "userArguments": flags,
                "tools": tools,
            },
        } })
        .build()?;

    let response: GodboltResponse = http.execute(request).await?.json().await?;

    // TODO: use the extract_relevant_lines utility to strip stderr nicely
    Ok(if response.code == 0 {
        Compilation::Success {
            asm: response.asm.full_with_ansi_codes_stripped()?,
            stderr: response.stderr.full_with_ansi_codes_stripped()?,
            llvm_mca: match response
                .tools
                .iter()
                .find(|tool| tool.id == LLVM_MCA_TOOL_ID)
            {
                Some(llvm_mca) => Some(llvm_mca.stdout.full_with_ansi_codes_stripped()?),
                None => None,
            },
        }
    } else {
        Compilation::Error {
            stderr: response.stderr.full_with_ansi_codes_stripped()?,
        }
    })
}

async fn save_to_shortlink(
    http: &reqwest::Client,
    code: &str,
    rustc: &str,
    flags: &str,
    run_llvm_mca: bool,
) -> Result<String, Error> {
    #[derive(serde::Deserialize)]
    struct GodboltShortenerResponse {
        url: String,
    }

    let tools = if run_llvm_mca {
        serde_json::json! {
            [{"id": LLVM_MCA_TOOL_ID}]
        }
    } else {
        serde_json::json! {
            []
        }
    };

    let response = http
        .post("https://godbolt.org/api/shortener")
        .json(&serde_json::json! { {
            "sessions": [{
                "language": "rust",
                "source": code,
                "compilers": [{
                    "id": rustc,
                    "options": flags,
                    "tools": tools,
                }],
            }]
        } })
        .send()
        .await?;

    Ok(response.json::<GodboltShortenerResponse>().await?.url)
}

#[derive(PartialEq, Clone, Copy)]
enum GodboltMode {
    Asm,
    LlvmIr,
    Mca,
}

async fn generic_godbolt(
    ctx: Context<'_>,
    params: poise::KeyValueArgs,
    code: poise::CodeBlock,
    mode: GodboltMode,
) -> Result<(), Error> {
    let run_llvm_mca = mode == GodboltMode::Mca;

    let (rustc, flags) = rustc_id_and_flags(ctx.data(), &params, mode).await?;

    let (lang, text);
    let mut note = String::new();

    let godbolt_result =
        compile_rust_source(&ctx.data().http, &code.code, &rustc, &flags, run_llvm_mca).await?;

    match godbolt_result {
        Compilation::Success {
            asm,
            stderr,
            llvm_mca,
        } => {
            lang = match mode {
                GodboltMode::Asm => "x86asm",
                GodboltMode::Mca => "rust",
                GodboltMode::LlvmIr => "llvm",
            };
            text = match mode {
                GodboltMode::Mca => {
                    let llvm_mca = llvm_mca.ok_or("No llvm-mca result was sent by Godbolt")?;
                    strip_llvm_mca_result(&llvm_mca).to_owned()
                }
                GodboltMode::Asm | GodboltMode::LlvmIr => asm,
            };
            if !stderr.is_empty() {
                note += "Note: compilation produced warnings\n";
            }
        }
        Compilation::Error { stderr } => {
            lang = "rust";
            text = stderr;
        }
    };

    if !code.code.contains("pub fn") {
        note += "Note: only public functions (`pub fn`) are shown\n";
    }

    if text.trim().is_empty() {
        ctx.say(format!("``` ```{}", note)).await?;
    } else {
        super::reply_potentially_long_text(
            ctx,
            &format!("```{}\n{}", lang, text),
            &format!("\n```{}", note),
            async {
                format!(
                    "Output too large. Godbolt link: <{}>",
                    save_to_shortlink(&ctx.data().http, &code.code, &rustc, &flags, run_llvm_mca)
                        .await
                        .unwrap_or_else(|e| {
                            log::warn!("failed to generate godbolt shortlink: {}", e);
                            "failed to retrieve".to_owned()
                        }),
                )
            },
        )
        .await?;
    }

    Ok(())
}

/// View assembly using Godbolt
///
/// Compile Rust code using <https://rust.godbolt.org>. Full optimizations are applied unless \
/// overriden.
/// ```
/// ?godbolt flags={} rustc={} ``​`
/// pub fn your_function() {
///     // Code
/// }
/// ``​`
/// ```
/// Optional arguments:
/// - `flags`: flags to pass to rustc invocation. Defaults to `"-Copt-level=3 --edition=2021"`
/// - `rustc`: compiler version to invoke. Defaults to `nightly`. Possible values: `nightly`, `beta` or full version like `1.45.2`
#[poise::command(prefix_command, broadcast_typing, track_edits, category = "Godbolt")]
pub async fn godbolt(
    ctx: Context<'_>,
    params: poise::KeyValueArgs,
    code: poise::CodeBlock,
) -> Result<(), Error> {
    generic_godbolt(ctx, params, code, GodboltMode::Asm).await
}

fn strip_llvm_mca_result(text: &str) -> &str {
    text[..text.find("Instruction Info").unwrap_or(text.len())].trim()
}

/// Run performance analysis using llvm-mca
///
/// Run the performance analysis tool llvm-mca using <https://rust.godbolt.org>. Full optimizations \
/// are applied unless overriden.
/// ```
/// ?mca flags={} rustc={} ``​`
/// pub fn your_function() {
///     // Code
/// }
/// ``​`
/// ```
/// Optional arguments:
/// - `flags`: flags to pass to rustc invocation. Defaults to `"-Copt-level=3 --edition=2021"`
/// - `rustc`: compiler version to invoke. Defaults to `nightly`. Possible values: `nightly`, `beta` or full version like `1.45.2`
#[poise::command(prefix_command, broadcast_typing, track_edits, category = "Godbolt")]
pub async fn mca(
    ctx: Context<'_>,
    params: poise::KeyValueArgs,
    code: poise::CodeBlock,
) -> Result<(), Error> {
    generic_godbolt(ctx, params, code, GodboltMode::Mca).await
}

/// View LLVM IR using Godbolt
///
/// Compile Rust code using <https://rust.godbolt.org> and emits LLVM IR. Full optimizations \
/// are applied unless overriden.
///
/// Equivalent to ?godbolt but with extra flags `--emit=llvm-ir -Cdebuginfo=0`.
/// ```
/// ?llvmir flags={} rustc={} ``​`
/// pub fn your_function() {
///     // Code
/// }
/// ``​`
/// ```
/// Optional arguments:
/// - `flags`: flags to pass to rustc invocation. Defaults to `"-Copt-level=3 --edition=2021"`
/// - `rustc`: compiler version to invoke. Defaults to `nightly`. Possible values: `nightly`, `beta` or full version like `1.45.2`
#[poise::command(prefix_command, broadcast_typing, track_edits, category = "Godbolt")]
pub async fn llvmir(
    ctx: Context<'_>,
    params: poise::KeyValueArgs,
    code: poise::CodeBlock,
) -> Result<(), Error> {
    generic_godbolt(ctx, params, code, GodboltMode::LlvmIr).await
}

// TODO: adjust doc
/// View difference between assembled functions
///
/// Compiles two Rust code snippets using <https://rust.godbolt.org> and diffs them. Full optimizations \
/// are applied unless overriden.
/// ```
/// ?asmdiff flags={} rustc={} ``​`
/// pub fn foo(x: u32) -> u32 {
///     x
/// }
/// ``​` ``​`
/// pub fn foo(x: u64) -> u64 {
///     x
/// }
/// ``​`
/// ```
/// Optional arguments:
/// - `flags`: flags to pass to rustc invocation. Defaults to `"-Copt-level=3 --edition=2021"`
/// - `rustc`: compiler version to invoke. Defaults to `nightly`. Possible values: `nightly`, `beta` or full version like `1.45.2`
#[poise::command(
    prefix_command,
    broadcast_typing,
    track_edits,
    hide_in_help,
    category = "Godbolt"
)]
pub async fn asmdiff(
    ctx: Context<'_>,
    params: poise::KeyValueArgs,
    code1: poise::CodeBlock,
    code2: poise::CodeBlock,
) -> Result<(), Error> {
    let (rustc, flags) = rustc_id_and_flags(ctx.data(), &params, GodboltMode::Asm).await?;

    let (asm1, asm2) = tokio::try_join!(
        compile_rust_source(&ctx.data().http, &code1.code, &rustc, &flags, false),
        compile_rust_source(&ctx.data().http, &code2.code, &rustc, &flags, false),
    )?;
    let result = match (asm1, asm2) {
        (Compilation::Success { asm: a, .. }, Compilation::Success { asm: b, .. }) => Ok((a, b)),
        (Compilation::Error { stderr }, _) => Err(stderr),
        (_, Compilation::Error { stderr }) => Err(stderr),
    };

    match result {
        Ok((asm1, asm2)) => {
            let mut path1 = std::env::temp_dir();
            path1.push("a");
            tokio::fs::write(&path1, asm1).await?;

            let mut path2 = std::env::temp_dir();
            path2.push("b");
            tokio::fs::write(&path2, asm2).await?;

            let diff = tokio::process::Command::new("git")
                .args(&["diff", "--no-index"])
                .arg(&path1)
                .arg(&path2)
                .output()
                .await?
                .stdout;

            super::reply_potentially_long_text(
                ctx,
                &format!("```diff\n{}", String::from_utf8_lossy(&diff)),
                "```",
                async { String::from("(output was truncated)") },
            )
            .await?;
        }
        Err(stderr) => {
            super::reply_potentially_long_text(
                ctx,
                &format!("```rust\n{}", stderr),
                "```",
                async { String::from("(output was truncated)") },
            )
            .await?;
        }
    }

    Ok(())
}

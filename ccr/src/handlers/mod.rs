pub mod aws;
pub mod brew;
pub mod cargo;
pub mod clippy;
pub mod json;
pub mod log;
pub mod curl;
pub mod diff;
pub mod docker;
pub mod env;
pub mod eslint;
pub mod find;
pub mod gh;
pub mod git;
pub mod go;
pub mod golangci_lint;
pub mod grep;
pub mod helm;
pub mod jest;
pub mod journalctl;
pub mod jq;
pub mod kubectl;
pub mod ls;
pub mod make;
pub mod maven;
pub mod next;
pub mod npm;
pub mod pip;
pub mod playwright;
pub mod pnpm;
pub mod prettier;
pub mod prisma;
pub mod psql;
pub mod pytest;
pub mod python;
pub mod read;
pub mod terraform;
pub mod tree;
pub mod tsc;
pub mod util;
pub mod vitest;

/// A specialized handler for a known command.
/// Handlers may inject extra flags (`rewrite_args`) and compact the output (`filter`).
pub trait Handler: Send + Sync {
    /// Optionally rewrite the argument list before execution (e.g. inject --message-format json).
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        args.to_vec()
    }

    /// Filter the combined stdout+stderr output into a compact representation.
    fn filter(&self, output: &str, args: &[String]) -> String;
}

/// Returns a handler for the given command name.
/// Falls through a three-level lookup:
/// 1. Exact registry match
/// 2. Static alias / pattern table (covers versioned binaries, wrappers, common aliases)
/// 3. BERT similarity routing for truly unknown commands
pub fn get_handler(cmd: &str) -> Option<Box<dyn Handler>> {
    // Level 0: user-defined TOML filters (.ccr/filters.toml or ~/.config/ccr/filters.toml)
    let user_filters = crate::user_filters::load_user_filters();
    if let Some(filter_def) = user_filters.commands.get(cmd) {
        return Some(Box::new(crate::user_filters::UserFilterHandler {
            filter_def: filter_def.clone(),
        }));
    }

    get_handler_exact(cmd)
        .or_else(|| get_handler_alias(cmd))
        .or_else(|| get_handler_bert(cmd))
}

fn get_handler_exact(cmd: &str) -> Option<Box<dyn Handler>> {
    match cmd {
        // Existing handlers
        "cargo" => Some(Box::new(cargo::CargoHandler)),
        "curl" => Some(Box::new(curl::CurlHandler)),
        "git" => Some(Box::new(git::GitHandler)),
        "docker" | "docker-compose" => Some(Box::new(docker::DockerHandler)),
        "npm" | "yarn" => Some(Box::new(npm::NpmHandler)),
        "pnpm" => Some(Box::new(pnpm::PnpmHandler)),
        "ls" => Some(Box::new(ls::LsHandler)),
        "cat" => Some(Box::new(read::ReadHandler)),
        "grep" | "rg" => Some(Box::new(grep::GrepHandler)),
        "find" => Some(Box::new(find::FindHandler)),
        // Batch 1: TypeScript / JavaScript
        "tsc" => Some(Box::new(tsc::TscHandler)),
        "vitest" => Some(Box::new(vitest::VitestHandler)),
        "jest" => Some(Box::new(jest::JestHandler)),
        "eslint" => Some(Box::new(eslint::EslintHandler)),
        // Batch 2: Python
        "pytest" => Some(Box::new(pytest::PytestHandler)),
        "pip" | "pip3" | "uv" => Some(Box::new(pip::PipHandler)),
        "python" | "python3" => Some(Box::new(python::PythonHandler)),
        // Batch 3: DevOps / Cloud
        "kubectl" => Some(Box::new(kubectl::KubectlHandler)),
        "gh" => Some(Box::new(gh::GhHandler)),
        "terraform" | "tofu" => Some(Box::new(terraform::TerraformHandler)),
        "aws" => Some(Box::new(aws::AwsHandler)),
        "make" | "gmake" => Some(Box::new(make::MakeHandler)),
        // Batch 4: System / Utility
        "psql" | "pgcli" => Some(Box::new(psql::PsqlHandler)),
        "tree" => Some(Box::new(tree::TreeHandler)),
        "diff" => Some(Box::new(diff::DiffHandler)),
        "jq" => Some(Box::new(jq::JqHandler)),
        "env" | "printenv" => Some(Box::new(env::EnvHandler)),
        // Batch 5: High-priority new handlers
        "go" => Some(Box::new(go::GoHandler)),
        "mvn" => Some(Box::new(maven::MavenHandler)),
        "gradle" | "./gradlew" | "gradlew" => Some(Box::new(maven::GradleHandler)),
        "brew" => Some(Box::new(brew::BrewHandler)),
        "helm" => Some(Box::new(helm::HelmHandler)),
        "journalctl" => Some(Box::new(journalctl::JournalctlHandler)),
        "json" => Some(Box::new(json::JsonHandler)),
        "log" => Some(Box::new(log::LogHandler)),
        // Batch 6: New handlers
        "clippy" | "cargo-clippy" => Some(Box::new(clippy::ClippyHandler)),
        "next" | "next.js" => Some(Box::new(next::NextHandler)),
        "playwright" => Some(Box::new(playwright::PlaywrightHandler)),
        "prisma" => Some(Box::new(prisma::PrismaHandler)),
        "golangci-lint" | "golangci_lint" => Some(Box::new(golangci_lint::GolangCiLintHandler)),
        "prettier" => Some(Box::new(prettier::PrettierHandler)),
        _ => None,
    }
}


// ── Level 2: Static alias / pattern routing ───────────────────────────────────

/// Maps command name patterns to canonical handler keys.
/// Covers versioned binaries, wrapper scripts, and well-known aliases.
const STATIC_ALIASES: &[(&str, &str)] = &[
    // Python variants
    ("python3.8",  "python"), ("python3.9",  "python"), ("python3.10", "python"),
    ("python3.11", "python"), ("python3.12", "python"), ("python3.13", "python"),
    ("py",         "python"),
    // pip variants
    ("pip3.9",  "pip"), ("pip3.10", "pip"), ("pip3.11", "pip"), ("pip3.12", "pip"),
    ("poetry",  "pip"), ("pdm",     "pip"), ("conda",   "pip"),
    // pytest variants
    ("py.test",  "pytest"), ("pytest3", "pytest"),
    // JS runtimes that run jest-style tests
    ("bun",  "jest"),
    ("deno", "jest"),
    // Build / task runners
    ("nx",          "jest"),   // nx test runs jest under the hood
    ("./gradlew",   "gradle"), ("gradlew",   "gradle"),
    ("./mvnw",      "mvn"),    ("mvnw",      "mvn"),
    ("ninja",       "make"),   ("bmake",     "make"),
    // Next.js variants
    ("next-router",     "next"),
    // Playwright variants
    ("npx playwright",  "playwright"),
    // Prettier variants
    ("prettier2",       "prettier"),
    // Go linter variants
    ("golangci",        "golangci-lint"),
    // Kubernetes wrappers
    ("k",           "kubectl"), ("kubectl.exe", "kubectl"),
    ("minikube",    "kubectl"), ("kind",        "kubectl"),
    // Helm variants
    ("helm3",             "helm"),
    ("pnpx",              "pnpm"),
    // Terraform variants
    ("terraform1",  "terraform"),
    // Cloud CLIs
    ("gcloud",      "aws"), // similar output pattern
    ("az",          "aws"),
];

fn get_handler_alias(cmd: &str) -> Option<Box<dyn Handler>> {
    // Exact alias lookup
    for &(alias, target) in STATIC_ALIASES {
        if alias == cmd {
            return get_handler_exact(target);
        }
    }
    // Pattern: any `python3.X` not in the static list
    if cmd.starts_with("python3.") || cmd.starts_with("python2.") {
        return get_handler_exact("python");
    }
    // Pattern: any `pip3.X`
    if cmd.starts_with("pip3.") || cmd.starts_with("pip2.") {
        return get_handler_exact("pip");
    }
    None
}

// ── Level 3: BERT similarity routing ─────────────────────────────────────────

/// Wraps an inner handler to disable rewrite_args (filter output only, no arg injection).
/// Used for MEDIUM confidence BERT routes where we're not confident enough to modify args.
struct FilterOnlyHandler {
    inner: Box<dyn Handler>,
}

impl Handler for FilterOnlyHandler {
    fn rewrite_args(&self, args: &[String]) -> Vec<String> {
        args.to_vec()  // no-op: don't modify args when confidence is medium
    }
    fn filter(&self, output: &str, args: &[String]) -> String {
        self.inner.filter(output, args)
    }
}

/// Handler representatives: (canonical_name, display_label) for embedding.
/// We embed the canonical name and compare against the unknown command.
const HANDLER_REPS: &[(&str, &str)] = &[
    ("cargo build test check clippy",   "cargo"),
    ("git commit push pull diff log",   "git"),
    ("docker run logs ps images build", "docker"),
    ("npm install test run script",     "npm"),
    ("kubectl get logs describe apply", "kubectl"),
    ("terraform plan apply init",       "terraform"),
    ("pytest test assertion failure",   "pytest"),
    ("jest test describe it expect",    "jest"),
    ("go build test run mod",           "go"),
    ("mvn build test install compile",  "mvn"),
    ("helm install upgrade list diff",  "helm"),
    ("brew install update upgrade",     "brew"),
    ("aws ec2 s3 lambda iam",           "aws"),
    ("make build clean install target", "make"),
    ("psql query select insert",        "psql"),
    ("next build dev lint start",       "next"),
    ("playwright test browser e2e",     "playwright"),
    ("prisma generate migrate schema",  "prisma"),
    ("golangci-lint run check issues",  "golangci-lint"),
    ("prettier format check write",     "prettier"),
    ("pnpm install add run exec",       "pnpm"),
    ("clippy warning lint rust",        "clippy"),
    ("json parse schema object array",  "json"),
    ("log output lines errors warnings","log"),
];

const BERT_THRESHOLD_HIGH: f32 = 0.70;  // Full handler (filter + rewrite_args)
const BERT_THRESHOLD_MED:  f32 = 0.55;  // Filter only (no rewrite_args)
const BERT_MARGIN_HIGH:    f32 = 0.15;  // Gap between top-1 and top-2 for HIGH
const BERT_MARGIN_MED:     f32 = 0.08;  // Gap between top-1 and top-2 for MED
const BERT_SUBCOMMAND_BOOST: f32 = 0.08; // Boost when subcommand matches known pattern

/// Maps known subcommand words to the handler labels they strongly suggest.
/// If `cmd` contains a space and the subcommand matches, apply a boost to that handler's score.
const SUBCOMMAND_HINTS: &[(&str, &str)] = &[
    // test-runner subcommands
    ("test",    "pytest"),
    ("test",    "jest"),
    ("test",    "vitest"),
    ("test",    "go"),
    // build tools
    ("build",   "cargo"),
    ("build",   "go"),
    ("build",   "docker"),
    ("build",   "next"),
    ("build",   "mvn"),
    // install / add
    ("install", "npm"),
    ("install", "pnpm"),
    ("install", "brew"),
    ("install", "pip"),
    ("install", "helm"),
    // run / exec
    ("run",     "go"),
    ("run",     "cargo"),
    ("run",     "docker"),
    // lint
    ("lint",    "eslint"),
    ("lint",    "golangci-lint"),
    ("lint",    "clippy"),
    ("check",   "cargo"),
    ("check",   "tsc"),
    // deploy / infra
    ("plan",    "terraform"),
    ("apply",   "terraform"),
    ("apply",   "kubectl"),
];

fn get_handler_bert(cmd: &str) -> Option<Box<dyn Handler>> {
    if cmd.contains('/') || cmd.starts_with('-') || cmd.is_empty() {
        return None;
    }

    // Split into binary and subcommand (e.g. "bloop test" → binary="bloop", sub=Some("test"))
    let mut parts = cmd.splitn(2, ' ');
    let binary = parts.next().unwrap_or(cmd);
    let subcommand = parts.next().unwrap_or("").to_lowercase();

    // Embed all rep phrases + the binary name
    let reps: Vec<&str> = HANDLER_REPS.iter().map(|(rep, _)| *rep).collect();
    let mut all_texts = reps.clone();
    all_texts.push(binary);  // embed just the binary, not the full cmd with args

    let embeddings = ccr_core::summarizer::embed_batch(&all_texts).ok()?;
    let cmd_emb = embeddings.last()?;
    let rep_embs = &embeddings[..embeddings.len() - 1];

    // Compute similarities with optional subcommand boost
    let mut scores: Vec<(usize, f32)> = rep_embs
        .iter()
        .enumerate()
        .map(|(i, emb)| {
            let mut sim = util::cosine_similarity(emb, cmd_emb);
            // Apply subcommand boost if the subcommand matches this handler
            if !subcommand.is_empty() {
                let target_label = HANDLER_REPS[i].1;
                let matches = SUBCOMMAND_HINTS
                    .iter()
                    .any(|(sub, lbl)| *sub == subcommand.as_str() && *lbl == target_label);
                if matches {
                    sim += BERT_SUBCOMMAND_BOOST;
                    sim = sim.min(1.0);
                }
            }
            (i, sim)
        })
        .collect();

    // Sort descending by score to find top-1 and top-2
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let (best_idx, best_sim) = scores[0];
    let second_sim = scores.get(1).map(|(_, s)| *s).unwrap_or(0.0);
    let margin = best_sim - second_sim;

    let target = HANDLER_REPS[best_idx].1;

    if best_sim >= BERT_THRESHOLD_HIGH && margin >= BERT_MARGIN_HIGH {
        // HIGH confidence: full handler including rewrite_args
        get_handler_exact(target)
    } else if best_sim >= BERT_THRESHOLD_MED && margin >= BERT_MARGIN_MED {
        // MEDIUM confidence: filter only, no arg rewriting
        get_handler_exact(target).map(|h| -> Box<dyn Handler> {
            Box::new(FilterOnlyHandler { inner: h })
        })
    } else {
        None
    }
}

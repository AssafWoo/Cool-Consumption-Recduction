use anyhow::Result;
use ccr_core::analytics::Analytics;
use owo_colors::{OwoColorize, Stream::Stdout, Style};
use std::collections::BTreeMap;

/// Pricing table for known Anthropic model families (input tokens, $/1M).
/// More-specific prefixes must appear before less-specific ones because
/// matching uses `.contains(prefix)` and stops at the first hit.
const MODEL_PRICES: &[(&str, f64)] = &[
    ("claude-opus-4-6",    5.00),  // Opus 4.6
    ("claude-opus-4-5",    5.00),  // Opus 4.5
    ("claude-opus-4",     15.00),  // Opus 4 / 4.1 (deprecated)
    ("claude-opus-3",     15.00),
    ("claude-sonnet-4",    3.00),
    ("claude-sonnet-3-7",  3.00),
    ("claude-sonnet-3-5",  3.00),
    ("claude-haiku-4-5",   1.00),  // Haiku 4.5
    ("claude-haiku-4",     0.80),
    ("claude-haiku-3",     0.25),
];

/// Resolve the price per token to use for cost estimates.
/// Priority: config override → ANTHROPIC_MODEL env var → $3.00 fallback.
fn resolve_price() -> (f64, String) {
    // 1. Config override
    if let Ok(cfg) = crate::config_loader::load_config() {
        if let Some(price) = cfg.global.cost_per_million_tokens {
            return (price / 1_000_000.0, format!("${:.2}/1M (config)", price));
        }
    }

    // 2. Auto-detect from ANTHROPIC_MODEL env var
    if let Ok(model) = std::env::var("ANTHROPIC_MODEL") {
        let model_lc = model.to_lowercase();
        for (prefix, price) in MODEL_PRICES {
            if model_lc.contains(prefix) {
                return (
                    price / 1_000_000.0,
                    format!("${:.2}/1M ({})", price, model),
                );
            }
        }
    }

    // 3. Fallback
    (3.00 / 1_000_000.0, "$3.00/1M (set ANTHROPIC_MODEL to auto-detect)".to_string())
}

pub fn run(history: bool, days: u32, breakdown: bool, insight: bool) -> Result<()> {
    let records = load_records()?;

    if insight {
        print_insight(&records, days);
    } else if history {
        print_history(&records, days);
    } else {
        print_summary(&records, breakdown);
    }

    Ok(())
}

// ─── Data loading ──────────────────────────────────────────────────────────────

fn load_records() -> Result<Vec<Analytics>> {
    // Load from SQLite (migrates from JSONL automatically on first call)
    crate::analytics_db::load_all(None)
}

// ─── Summary view (default) ────────────────────────────────────────────────────

fn print_summary(records: &[Analytics], breakdown: bool) {
    // Split legacy (timestamp=0) records from dated ones.
    // Legacy records have no timestamp and cannot be placed in any date window.
    let (legacy, dated): (Vec<&Analytics>, Vec<&Analytics>) =
        records.iter().partition(|r| r.timestamp_secs == 0);

    let total_input: usize = records.iter().map(|r| r.input_tokens).sum();
    let total_output: usize = records.iter().map(|r| r.output_tokens).sum();
    let total_saved = total_input.saturating_sub(total_output);
    let overall_pct = savings_pct(total_input, total_output);
    let (price_per_token, price_label) = resolve_price();
    let cost_saved = total_saved as f64 * price_per_token;

    let now_secs = now_unix();
    let today_start = day_start(now_secs);
    let week_start = now_secs.saturating_sub(7 * 86400);

    let today: Vec<&Analytics> = dated
        .iter()
        .copied()
        .filter(|r| r.timestamp_secs >= today_start)
        .collect();
    let week: Vec<&Analytics> = dated
        .iter()
        .copied()
        .filter(|r| r.timestamp_secs >= week_start)
        .collect();

    // ── Header ──
    let total_exec_ms: u64 = records.iter().filter_map(|r| r.duration_ms).sum();
    let timed_runs = records.iter().filter(|r| r.duration_ms.is_some()).count();
    let avg_ms: Option<u64> = if timed_runs > 0 {
        Some(total_exec_ms / timed_runs as u64)
    } else {
        None
    };
    let savings_bar = {
        let filled = ((overall_pct / 100.0) * 24.0) as usize;
        let empty = 24usize.saturating_sub(filled);
        format!("{}{}", "█".repeat(filled), "░".repeat(empty))
    };

    println!("{}", "CCR Token Savings".if_supports_color(Stdout, |t| t.bold()));
    println!("{}", "═".repeat(49).if_supports_color(Stdout, |t| t.dimmed()));
    let green_bold = Style::new().bold().green();
    let yellow_bold = Style::new().bold().yellow();

    // "Runs: 206  (avg 87ms)"
    let runs_suffix = avg_ms
        .map(|ms| format!("  (avg {}ms)", ms))
        .unwrap_or_default();
    println!(
        "  Runs:           {}{}",
        records.len(),
        runs_suffix.if_supports_color(Stdout, |t| t.dimmed()),
    );

    // "Tokens saved: 27.3k / 46.7k  (54.4%)  ████████░░░░░░"
    println!(
        "  Tokens saved:   {} / {}  ({})  {}",
        fmt_tokens(total_saved).if_supports_color(Stdout, |t| t.style(green_bold)),
        fmt_tokens(total_input).if_supports_color(Stdout, |t| t.dimmed()),
        format!("{:.1}%", overall_pct).if_supports_color(Stdout, |t| t.green()),
        savings_bar.if_supports_color(Stdout, |t| t.green()),
    );

    println!(
        "  Cost saved:     {}  {}",
        format!("~{}", fmt_cost(cost_saved)).if_supports_color(Stdout, |t| t.style(yellow_bold)),
        format!("(at {})", price_label).if_supports_color(Stdout, |t| t.dimmed()),
    );
    if !legacy.is_empty() {
        let legacy_saved: usize = legacy.iter().map(|r| r.tokens_saved()).sum();
        println!(
            "  {}",
            format!(
                "(includes {} legacy run{} without timestamps · {} tokens)",
                legacy.len(),
                if legacy.len() == 1 { "" } else { "s" },
                fmt_tokens(legacy_saved)
            )
            .if_supports_color(Stdout, |t| t.dimmed()),
        );
    }

    if !today.is_empty() {
        let t_saved: usize = today.iter().map(|r| r.tokens_saved()).sum();
        let t_in: usize = today.iter().map(|r| r.input_tokens).sum();
        let t_out: usize = today.iter().map(|r| r.output_tokens).sum();
        let t_cost = t_saved as f64 * price_per_token;
        println!(
            "  Today:          {} runs · {} saved · {} · {}",
            today.len(),
            fmt_tokens(t_saved).if_supports_color(Stdout, |t| t.cyan()),
            format!("{:.1}%", savings_pct(t_in, t_out)).if_supports_color(Stdout, |t| t.cyan()),
            format!("~{}", fmt_cost(t_cost)).if_supports_color(Stdout, |t| t.yellow()),
        );
    }
    if week.len() > today.len() {
        let w_saved: usize = week.iter().map(|r| r.tokens_saved()).sum();
        let w_in: usize = week.iter().map(|r| r.input_tokens).sum();
        let w_out: usize = week.iter().map(|r| r.output_tokens).sum();
        let w_cost = w_saved as f64 * price_per_token;
        println!(
            "  7-day:          {} runs · {} saved · {} · {}",
            week.len(),
            fmt_tokens(w_saved).if_supports_color(Stdout, |t| t.cyan()),
            format!("{:.1}%", savings_pct(w_in, w_out)).if_supports_color(Stdout, |t| t.cyan()),
            format!("~{}", fmt_cost(w_cost)).if_supports_color(Stdout, |t| t.yellow()),
        );
    }

    // ── Top command ──
    if !records.is_empty() {
        let mut top_by_cmd: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for r in records {
            let key = normalize_cmd_key(r.command.as_deref());
            let e = top_by_cmd.entry(key).or_default();
            e.0 += r.input_tokens;
            e.1 += r.output_tokens;
        }
        if let Some((top_cmd, (top_in, top_out))) = top_by_cmd
            .into_iter()
            .max_by_key(|(_, (i, o))| i.saturating_sub(*o))
        {
            let top_saved = top_in.saturating_sub(top_out);
            let top_pct = savings_pct(top_in, top_out);
            println!(
                "  Top command:    {}  {}  ·  {} saved",
                top_cmd.if_supports_color(Stdout, |t| t.bold()),
                format!("{:.1}%", top_pct).if_supports_color(Stdout, |t| t.green()),
                fmt_tokens(top_saved).if_supports_color(Stdout, |t| t.green()),
            );
        }
    }

    if records.is_empty() {
        return;
    }

    // ── Per-command table (only with --breakdown) ──
    if breakdown {
        println!();
        println!("{}", "Per-Command Breakdown".if_supports_color(Stdout, |t| t.bold()));

        let mut by_cmd: BTreeMap<String, CmdStats> = BTreeMap::new();
        for r in records {
            let key = normalize_cmd_key(r.command.as_deref());
            let entry = by_cmd.entry(key).or_default();
            entry.input += r.input_tokens;
            entry.output += r.output_tokens;
            entry.count += 1;
            if let Some(ms) = r.duration_ms {
                entry.total_ms += ms;
                entry.ms_count += 1;
            }
        }

        let mut rows: Vec<(String, CmdStats)> = by_cmd.into_iter().collect();
        rows.sort_by(|a, b| b.1.saved().cmp(&a.1.saved()));

        let col_w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(7).max(7);
        let sep = "─".repeat(col_w + 51);
        println!("{}", sep.if_supports_color(Stdout, |t| t.dimmed()));
        println!(
            "{}",
            format!(
                "{:<col_w$} {:>6}  {:>10}  {:>8}  {:>7}  {}",
                "COMMAND", "RUNS", "SAVED", "SAVINGS", "AVG ms", "IMPACT",
                col_w = col_w
            )
            .if_supports_color(Stdout, |t| t.bold())
        );
        println!("{}", sep.if_supports_color(Stdout, |t| t.dimmed()));

        for (cmd, stats) in &rows {
            let pct = savings_pct(stats.input, stats.output);
            let avg_ms = if stats.ms_count > 0 {
                format!("{:>6}", stats.total_ms / stats.ms_count)
            } else {
                "     —".to_string()
            };
            let bar_len = (pct / 5.0) as usize;
            let bar = "█".repeat(bar_len.min(20));
            let dim_row = pct < 1.0;
            let bar_colored = if pct >= 40.0 {
                bar.if_supports_color(Stdout, |t| t.green()).to_string()
            } else if pct >= 15.0 {
                bar.if_supports_color(Stdout, |t| t.yellow()).to_string()
            } else {
                bar.if_supports_color(Stdout, |t| t.dimmed()).to_string()
            };
            let line = format!(
                "{:<col_w$} {:>6}  {:>10}  {:>7.1}%  {}  {}",
                cmd,
                stats.count,
                fmt_tokens(stats.saved()),
                pct,
                avg_ms,
                bar_colored,
                col_w = col_w
            );
            if dim_row {
                println!("{}", line.if_supports_color(Stdout, |t| t.dimmed()));
            } else {
                println!("{}", line);
            }
        }
    } else {
        println!(
            "  {}",
            "Run `ccr gain --breakdown` for per-command details."
                .if_supports_color(Stdout, |t| t.dimmed()),
        );
    }

    // ── Missed opportunities (from discover) ──
    let opportunities = crate::cmd::discover::top_unoptimized(5);
    if !opportunities.is_empty() {
        let total_potential: usize = opportunities.iter().map(|(_, t)| t).sum();
        if total_potential >= 2_000 {
            println!();
            let yellow_bold = Style::new().bold().yellow();
            println!("{}", "Unoptimized Commands".if_supports_color(Stdout, |t| t.style(yellow_bold)));
            println!("{}", format!("  Run `ccr discover` for full details · ~{} tokens potential",
                fmt_tokens(total_potential)
            ).if_supports_color(Stdout, |t| t.dimmed()));
            for (cmd, saveable) in &opportunities {
                println!("  {:<14} ~{} saveable",
                    cmd.if_supports_color(Stdout, |t| t.yellow()),
                    fmt_tokens(*saveable).if_supports_color(Stdout, |t| t.yellow()),
                );
            }
        }
    }
}

// ─── History view (--history) ─────────────────────────────────────────────────

fn print_history(records: &[Analytics], days: u32) {
    let (price_per_token, _) = resolve_price();
    let now_secs = now_unix();
    // Align cutoff to UTC midnight of the earliest displayed day so the rolling
    // window boundary doesn't split a calendar day and silently drop records.
    let first_day_ts = now_secs.saturating_sub((days as u64 - 1) * 86400);
    let cutoff = first_day_ts - (first_day_ts % 86400);

    // Group by calendar day (UTC date string "YYYY-MM-DD")
    let mut by_day: BTreeMap<String, DayStats> = BTreeMap::new();

    for r in records.iter().filter(|r| r.timestamp_secs > 0 && r.timestamp_secs >= cutoff) {
        let day = unix_to_date(r.timestamp_secs);
        let entry = by_day.entry(day).or_default();
        entry.input += r.input_tokens;
        entry.output += r.output_tokens;
        entry.count += 1;
    }

    // Fill gaps so every day in range appears
    for offset in 0..days {
        let ts = now_secs.saturating_sub(offset as u64 * 86400);
        let day = unix_to_date(ts);
        by_day.entry(day).or_default();
    }

    // Sort descending (most recent first)
    let mut rows: Vec<(String, DayStats)> = by_day.into_iter().collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    rows.truncate(days as usize);

    println!("{}", format!("CCR Daily History  (last {} days)", days).if_supports_color(Stdout, |t| t.bold()));
    println!("{}", "═".repeat(60).if_supports_color(Stdout, |t| t.dimmed()));

    let sep = "─".repeat(60);
    println!("{}", sep.if_supports_color(Stdout, |t| t.dimmed()));
    println!(
        "{}",
        format!(
            "{:<12}  {:>5}  {:>12}  {:>8}  {:>10}",
            "DATE", "RUNS", "SAVED", "SAVINGS", "COST SAVED"
        )
        .if_supports_color(Stdout, |t| t.bold())
    );
    println!("{}", sep.if_supports_color(Stdout, |t| t.dimmed()));

    let mut total_input: usize = 0;
    let mut total_output: usize = 0;
    let mut total_count: usize = 0;

    for (day, stats) in &rows {
        let pct = savings_pct(stats.input, stats.output);
        let cost = stats.saved() as f64 * price_per_token;
        let saved_str = if stats.count == 0 {
            "—".to_string()
        } else {
            fmt_tokens(stats.saved())
        };
        let pct_str = if stats.count == 0 {
            "—".to_string()
        } else {
            format!("{:.1}%", pct)
        };
        let cost_str = if stats.count == 0 {
            "—".to_string()
        } else {
            fmt_cost(cost)
        };
        let dim_row = stats.count == 0;
        let line = format!(
            "{:<12}  {:>5}  {:>12}  {:>8}  {:>10}",
            day, stats.count, saved_str, pct_str, cost_str
        );
        if dim_row {
            println!("{}", line.if_supports_color(Stdout, |t| t.dimmed()));
        } else {
            println!("{}", line);
        }
        total_input += stats.input;
        total_output += stats.output;
        total_count += stats.count;
    }

    println!("{}", sep.if_supports_color(Stdout, |t| t.dimmed()));
    let total_saved = total_input.saturating_sub(total_output);
    let total_cost = total_saved as f64 * price_per_token;
    println!(
        "{}",
        format!(
            "{:<12}  {:>5}  {:>12}  {:>8}  {:>10}",
            format!("{}-day total", days),
            total_count,
            fmt_tokens(total_saved),
            format!("{:.1}%", savings_pct(total_input, total_output)),
            fmt_cost(total_cost)
        )
        .if_supports_color(Stdout, |t| t.bold())
    );

    // Legacy records (timestamp=0): show totals separately
    let legacy_iter = records.iter().filter(|r| r.timestamp_secs == 0);
    let (legacy_count, legacy_saved) = legacy_iter.fold((0usize, 0usize), |(c, s), r| {
        (c + 1, s + r.tokens_saved())
    });
    if legacy_count > 0 {
        let line = format!(
            "{:<12}  {:>5}  {:>12}  {:>8}  {:>10}",
            "(legacy)",
            legacy_count,
            fmt_tokens(legacy_saved),
            "—",
            "—",
        );
        println!("{}", line.if_supports_color(Stdout, |t| t.dimmed()));
    }

    // Top commands over the period
    let mut cmd_stats: BTreeMap<String, CmdStats> = BTreeMap::new();
    for r in records.iter().filter(|r| r.timestamp_secs > 0 && r.timestamp_secs >= cutoff) {
        let key = normalize_cmd_key(r.command.as_deref());
        let e = cmd_stats.entry(key).or_default();
        e.input += r.input_tokens;
        e.output += r.output_tokens;
        e.count += 1;
    }
    if !cmd_stats.is_empty() {
        let mut cmd_rows: Vec<(String, CmdStats)> = cmd_stats.into_iter().collect();
        cmd_rows.sort_by(|a, b| b.1.saved().cmp(&a.1.saved()));

        println!();
        println!("{}", "Top Commands".if_supports_color(Stdout, |t| t.bold()));
        println!("{}", "─".repeat(42).if_supports_color(Stdout, |t| t.dimmed()));
        println!("{}", format!("{:<14} {:>5}  {:>10}  {:>7}", "COMMAND", "RUNS", "SAVED", "SAVINGS").if_supports_color(Stdout, |t| t.bold()));
        println!("{}", "─".repeat(42).if_supports_color(Stdout, |t| t.dimmed()));
        for (cmd, s) in cmd_rows.iter().take(8) {
            println!(
                "{:<14} {:>5}  {:>10}  {:>6.1}%",
                cmd,
                s.count,
                fmt_tokens(s.saved()),
                savings_pct(s.input, s.output)
            );
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Normalize a stored command key for display:
/// - Strip leading "rtk " wrapper (e.g. "rtk git status" → "git status")
/// - Skip leading KEY=VALUE env var assignments (e.g. "GIT_COMMITTER_NAME=Assaf git status")
/// - Strip "rtk " wrapper prefix
/// - Take the basename of the first token (e.g. "/usr/bin/git status" → "git status")
/// - Collapse tool-event labels like "(read)" and "(glob)" into "(pipeline)"
fn normalize_cmd_key(raw: Option<&str>) -> String {
    let s = match raw {
        None => return "(pipeline)".to_string(),
        Some(s) => s,
    };
    // Collapse tool-event labels and bare wrapper names into (pipeline)
    if s == "(read)" || s == "(glob)" || s == "rtk" || s == "ccr" {
        return "(pipeline)".to_string();
    }
    // Skip leading KEY=VALUE env var assignments
    fn is_env_assign(t: &str) -> bool {
        let eq = t.find('=').unwrap_or(0);
        eq > 0 && t[..eq].chars().all(|c| c.is_ascii_uppercase() || c == '_')
    }
    let s: String = {
        let iter = s.split_whitespace().skip_while(|t| is_env_assign(t));
        iter.collect::<Vec<_>>().join(" ")
    };
    let s = s.as_str();
    if s.is_empty() {
        return "(pipeline)".to_string();
    }
    // Strip "rtk " prefix
    let s = s.strip_prefix("rtk ").unwrap_or(s);
    // Normalize basename of the first token
    let mut tokens = s.splitn(2, ' ');
    let first = tokens.next().unwrap_or(s);
    let rest = tokens.next().unwrap_or("");
    let basename = std::path::Path::new(first)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(first);
    if rest.is_empty() {
        basename.to_string()
    } else {
        format!("{} {}", basename, rest)
    }
}

#[derive(Default)]
struct CmdStats {
    input: usize,
    output: usize,
    count: usize,
    total_ms: u64,
    ms_count: u64,
}

impl CmdStats {
    fn saved(&self) -> usize {
        self.input.saturating_sub(self.output)
    }
}

#[derive(Default)]
struct DayStats {
    input: usize,
    output: usize,
    count: usize,
}

impl DayStats {
    fn saved(&self) -> usize {
        self.input.saturating_sub(self.output)
    }
}

fn savings_pct(input: usize, output: usize) -> f32 {
    if input == 0 {
        return 0.0;
    }
    let saved = input.saturating_sub(output);
    (saved as f32 / input as f32) * 100.0
}

fn fmt_cost(dollars: f64) -> String {
    if dollars < 0.0001 {
        format!("<$0.0001")
    } else if dollars < 0.01 {
        format!("${:.4}", dollars)
    } else if dollars < 1.0 {
        format!("${:.3}", dollars)
    } else {
        format!("${:.2}", dollars)
    }
}

fn fmt_duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{}ms", ms)
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1_000;
        format!("{}m {}s", mins, secs)
    }
}

fn fmt_tokens(n: usize) -> String {
    if n < 1_000 {
        format!("{}", n)
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Returns the unix timestamp of midnight (UTC) for the day containing `ts`.
fn day_start(ts: u64) -> u64 {
    ts - (ts % 86400)
}

/// Format a unix timestamp as "YYYY-MM-DD" (UTC).
fn unix_to_date(ts: u64) -> String {
    // Simple manual conversion — no chrono dep needed
    let secs = ts;
    let days = secs / 86400;

    // Days since Unix epoch (1970-01-01)
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", year, month, day)
}

/// Convert days-since-epoch to (year, month, day) using the proleptic Gregorian calendar.
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Adapted from a well-known public domain algorithm
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Format a unix timestamp as "Mon DD" (UTC), e.g. "Apr 05".
fn unix_to_month_day(ts: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (_, month, day) = days_to_ymd(ts / 86400);
    let m = MONTHS[(month.saturating_sub(1).min(11)) as usize];
    format!("{} {:02}", m, day)
}

// ─── Insight view (--insight) ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum SavingsCategory {
    NoiseReduction,
    BuildFiltering,
    PipelineSavings,
    CommandCompression,
}

#[derive(Default)]
struct CategoryStats {
    saved: usize,
    input: usize,
    count: usize,
    /// command name → run count within this category
    commands: std::collections::BTreeMap<String, usize>,
    cache_hits: usize,
}

fn categorize(cmd: Option<&str>, cache_hit: bool) -> SavingsCategory {
    if cache_hit {
        return SavingsCategory::PipelineSavings;
    }
    let raw = match cmd {
        None => return SavingsCategory::PipelineSavings,
        Some(s) => s,
    };
    // Check pipeline event markers first (these are exact stored values)
    let first_token = raw.split_whitespace().next().unwrap_or(raw);
    match first_token {
        "(read)" | "(read-level)" | "(glob)" | "(grep-tool)" | "(pipeline)" => {
            return SavingsCategory::PipelineSavings;
        }
        _ => {}
    }
    // Normalize to get the effective command name (handles env vars, paths, rtk prefix)
    let normalized = normalize_cmd_key(Some(raw));
    let cmd_name = normalized.split_whitespace().next().unwrap_or(&normalized);
    match cmd_name {
        "(pipeline)" => SavingsCategory::PipelineSavings,
        "find" | "ls" | "tree" => SavingsCategory::NoiseReduction,
        "cargo" | "go" | "npm" | "yarn" | "pnpm" | "make" | "gmake"
        | "mvn" | "gradle" | "pytest" | "jest" | "vitest" | "rspec"
        | "tsc" | "ruff" | "mypy" | "rubocop" | "eslint" | "biome"
        | "playwright" | "nx" | "turbo" | "uv" | "pip" | "rake"
        | "ember" | "next" | "webpack" | "vite" | "stylelint" | "prettier" => {
            SavingsCategory::BuildFiltering
        }
        _ => SavingsCategory::CommandCompression,
    }
}

fn format_save_label(r: &Analytics) -> String {
    let cmd = normalize_cmd_key(r.command.as_deref());
    match r.subcommand.as_deref() {
        Some(sub) if !sub.is_empty() => {
            let sub_display = if sub.len() > 30 {
                format!("\u{2026}{}", &sub[sub.len() - 28..])
            } else {
                sub.to_string()
            };
            format!("{} {}", cmd, sub_display)
        }
        _ => cmd,
    }
}

/// Aggregate windowed records into per-category stats and a time-ordered list.
/// Returns (category_map, windowed_records_sorted_by_tokens_saved_desc).
fn aggregate_by_category(
    records: &[Analytics],
    cutoff: u64,
) -> (BTreeMap<SavingsCategory, CategoryStats>, Vec<usize>) {
    // Collect indices of records that pass the window filter
    let windowed_indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.timestamp_secs > 0 && r.timestamp_secs >= cutoff)
        .map(|(i, _)| i)
        .collect();

    let mut by_category: BTreeMap<SavingsCategory, CategoryStats> = BTreeMap::new();
    for &i in &windowed_indices {
        let r = &records[i];
        let cat = categorize(r.command.as_deref(), r.cache_hit);
        let stats = by_category.entry(cat).or_default();
        stats.saved += r.tokens_saved();
        stats.input += r.input_tokens;
        stats.count += 1;
        let cmd_key = normalize_cmd_key(r.command.as_deref());
        *stats.commands.entry(cmd_key).or_default() += 1;
        if r.cache_hit {
            stats.cache_hits += 1;
        }
    }

    // Sort indices by tokens_saved descending for top-saves list
    let mut sorted = windowed_indices;
    sorted.sort_by(|&a, &b| records[b].tokens_saved().cmp(&records[a].tokens_saved()));

    (by_category, sorted)
}

fn print_insight(records: &[Analytics], days: u32) {
    let now_secs = now_unix();
    let first_day_ts = now_secs.saturating_sub((days as u64).saturating_sub(1) * 86400);
    let cutoff = first_day_ts - (first_day_ts % 86400);

    println!(
        "{}",
        format!("Your token savings  (last {} days)", days)
            .if_supports_color(Stdout, |t| t.bold())
    );
    println!("{}", "═".repeat(55).if_supports_color(Stdout, |t| t.dimmed()));
    println!();

    let (by_category, sorted_indices) = aggregate_by_category(records, cutoff);

    if sorted_indices.is_empty() {
        println!("  No token savings recorded in this period.");
        return;
    }

    let total_saved: usize = sorted_indices.iter().map(|&i| records[i].tokens_saved()).sum();

    // Render categories in fixed order
    let category_order: &[(SavingsCategory, &str)] = &[
        (SavingsCategory::NoiseReduction, "Noise reduction"),
        (SavingsCategory::BuildFiltering, "Build filtering"),
        (SavingsCategory::PipelineSavings, "Pipeline savings"),
        (SavingsCategory::CommandCompression, "Command compression"),
    ];

    for (cat, label) in category_order {
        if let Some(stats) = by_category.get(cat) {
            let pct = if total_saved > 0 {
                stats.saved as f32 / total_saved as f32 * 100.0
            } else {
                0.0
            };

            // Top 3 commands by run count
            let mut cmd_list: Vec<(&String, &usize)> = stats.commands.iter().collect();
            cmd_list.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
            let shown = cmd_list.len().min(3);
            let extra = cmd_list.len().saturating_sub(3);
            let mut cmd_parts: Vec<String> = cmd_list[..shown]
                .iter()
                .map(|(k, v)| format!("{} \u{00d7} {}", k, v))
                .collect();
            if extra > 0 {
                cmd_parts.push(format!("+{} more", extra));
            }
            let mut cmd_str = cmd_parts.join(", ");
            if *cat == SavingsCategory::PipelineSavings && stats.cache_hits > 0 {
                let s = if stats.cache_hits == 1 { "" } else { "s" };
                cmd_str.push_str(&format!(", incl. {} cache hit{}", stats.cache_hits, s));
            }

            let saved_str = fmt_tokens(stats.saved);
            let pct_str = format!("{:.0}%", pct);
            let line = format!(
                "  {:<22} {:>6}  {:>4}   {}",
                label, saved_str, pct_str, cmd_str
            );

            if pct >= 40.0 {
                println!("{}", line.if_supports_color(Stdout, |t| t.green()));
            } else if pct >= 15.0 {
                println!("{}", line.if_supports_color(Stdout, |t| t.yellow()));
            } else {
                println!("{}", line.if_supports_color(Stdout, |t| t.dimmed()));
            }
        }
    }

    println!();

    // Top 5 saves
    let top_n = sorted_indices.len().min(5);
    if top_n > 0 {
        println!("  Top {} saves:", top_n);
        for &i in &sorted_indices[..top_n] {
            let r = &records[i];
            let label = format_save_label(r);
            let date = unix_to_month_day(r.timestamp_secs);
            let saved = fmt_tokens(r.tokens_saved());
            println!("    {:<32} {:<8}  {:>6}", label, date, saved);
        }
    }

    println!();
    println!(
        "  Total saved: {}",
        fmt_tokens(total_saved).if_supports_color(Stdout, |t| t.green())
    );
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ccr_core::analytics::Analytics;

    fn make_record(
        cmd: Option<&str>,
        sub: Option<&str>,
        input: usize,
        output: usize,
        ts: u64,
        cache_hit: bool,
    ) -> Analytics {
        Analytics {
            input_tokens: input,
            output_tokens: output,
            savings_pct: if input > 0 {
                (input.saturating_sub(output) as f32 / input as f32) * 100.0
            } else {
                0.0
            },
            command: cmd.map(|s| s.to_string()),
            timestamp_secs: ts,
            subcommand: sub.map(|s| s.to_string()),
            duration_ms: None,
            cache_hit,
            agent: None,
        }
    }

    // ── categorize() ──────────────────────────────────────────────────────────

    #[test]
    fn categorize_find_is_noise() {
        assert_eq!(categorize(Some("find"), false), SavingsCategory::NoiseReduction);
    }

    #[test]
    fn categorize_cargo_is_build() {
        assert_eq!(categorize(Some("cargo"), false), SavingsCategory::BuildFiltering);
    }

    #[test]
    fn categorize_pipeline_markers() {
        for marker in &["(read)", "(read-level)", "(glob)", "(grep-tool)", "(pipeline)"] {
            assert_eq!(
                categorize(Some(marker), false),
                SavingsCategory::PipelineSavings,
                "expected PipelineSavings for {}",
                marker
            );
        }
    }

    #[test]
    fn categorize_cache_hit_overrides_cmd() {
        assert_eq!(
            categorize(Some("git"), true),
            SavingsCategory::PipelineSavings
        );
    }

    #[test]
    fn categorize_git_is_command_compression() {
        assert_eq!(
            categorize(Some("git"), false),
            SavingsCategory::CommandCompression
        );
    }

    #[test]
    fn categorize_unknown_cmd_is_command_compression() {
        assert_eq!(
            categorize(Some("zzz_unknown_tool"), false),
            SavingsCategory::CommandCompression
        );
    }

    #[test]
    fn categorize_none_cmd_is_pipeline() {
        assert_eq!(categorize(None, false), SavingsCategory::PipelineSavings);
    }

    // ── format_save_label() ───────────────────────────────────────────────────

    #[test]
    fn label_with_subcommand() {
        let r = make_record(Some("cargo"), Some("test"), 1000, 500, 1_700_000_000, false);
        assert_eq!(format_save_label(&r), "cargo test");
    }

    #[test]
    fn label_without_subcommand() {
        let r = make_record(Some("git"), None, 1000, 500, 1_700_000_000, false);
        assert_eq!(format_save_label(&r), "git");
    }

    #[test]
    fn label_long_path_truncated() {
        let long_sub = "a".repeat(40);
        let r = make_record(Some("find"), Some(&long_sub), 1000, 500, 1_700_000_000, false);
        let label = format_save_label(&r);
        // Should be "find …<last 28 chars>"
        assert!(label.starts_with("find "));
        // Total subcommand display should be "…" + 28 chars = 29 chars (+ "find " prefix)
        let sub_part = &label["find ".len()..];
        assert!(sub_part.starts_with('\u{2026}'));
        assert!(sub_part.len() <= 32); // ellipsis + 28 bytes
    }

    #[test]
    fn label_pipeline_no_sub() {
        let r = make_record(Some("(read)"), None, 1000, 500, 1_700_000_000, false);
        // normalize_cmd_key("(read)") → "(pipeline)"
        assert_eq!(format_save_label(&r), "(pipeline)");
    }

    // ── aggregation / integration ─────────────────────────────────────────────

    #[test]
    fn insight_aggregates_by_category() {
        let records = vec![
            make_record(Some("find"), None,  1000, 200, 1_700_000_000, false), // noise: 800
            make_record(Some("find"), None,  500,  100, 1_700_000_001, false), // noise: 400
            make_record(Some("cargo"), None, 2000, 500, 1_700_000_002, false), // build: 1500
            make_record(Some("git"), None,   1000, 600, 1_700_000_003, false), // compression: 400
            make_record(Some("(read)"), None, 800, 200, 1_700_000_004, false), // pipeline: 600
        ];
        let (by_cat, _) = aggregate_by_category(&records, 0);

        assert_eq!(by_cat[&SavingsCategory::NoiseReduction].saved, 1200);
        assert_eq!(by_cat[&SavingsCategory::BuildFiltering].saved, 1500);
        assert_eq!(by_cat[&SavingsCategory::CommandCompression].saved, 400);
        assert_eq!(by_cat[&SavingsCategory::PipelineSavings].saved, 600);
    }

    #[test]
    fn insight_top_saves_sorted_correctly() {
        let records = vec![
            make_record(Some("git"), None,   1000, 800, 1_700_000_000, false), // saved: 200
            make_record(Some("cargo"), None, 5000, 500, 1_700_000_001, false), // saved: 4500
            make_record(Some("find"), None,  2000, 100, 1_700_000_002, false), // saved: 1900
            make_record(Some("npm"), None,   3000, 800, 1_700_000_003, false), // saved: 2200
            make_record(Some("grep"), None,  1000, 400, 1_700_000_004, false), // saved: 600
        ];
        let (_, sorted) = aggregate_by_category(&records, 0);
        assert_eq!(sorted.len(), 5);
        // First index should be the record with most tokens saved (cargo = 4500)
        assert_eq!(records[sorted[0]].tokens_saved(), 4500);
        // Second: npm = 2200
        assert_eq!(records[sorted[1]].tokens_saved(), 2200);
        // Third: find = 1900
        assert_eq!(records[sorted[2]].tokens_saved(), 1900);
    }

    #[test]
    fn insight_empty_window_no_panic() {
        // All records have timestamp_secs = 0, so none pass the filter
        let records = vec![
            make_record(Some("cargo"), None, 1000, 500, 0, false),
        ];
        // Should not panic; returns empty aggregation
        let (by_cat, sorted) = aggregate_by_category(&records, 1);
        assert!(by_cat.is_empty());
        assert!(sorted.is_empty());
        // Also test print_insight with empty slice directly
        print_insight(&[], 7);
    }

    #[test]
    fn insight_cache_hit_counted_in_pipeline() {
        let records = vec![
            make_record(Some("git"), None, 1000, 300, 1_700_000_000, true), // cache_hit=true
        ];
        let (by_cat, _) = aggregate_by_category(&records, 0);

        // Should be in PipelineSavings, not CommandCompression
        assert!(by_cat.contains_key(&SavingsCategory::PipelineSavings));
        assert!(!by_cat.contains_key(&SavingsCategory::CommandCompression));
        assert_eq!(by_cat[&SavingsCategory::PipelineSavings].cache_hits, 1);
        assert_eq!(by_cat[&SavingsCategory::PipelineSavings].saved, 700);
    }
}

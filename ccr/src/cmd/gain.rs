use anyhow::Result;
use ccr_core::analytics::Analytics;
use std::collections::BTreeMap;

/// Pricing table for known Anthropic model families (input tokens, $/1M).
const MODEL_PRICES: &[(&str, f64)] = &[
    ("claude-opus-4",     15.00),
    ("claude-opus-3",     15.00),
    ("claude-sonnet-4",    3.00),
    ("claude-sonnet-3-7",  3.00),
    ("claude-sonnet-3-5",  3.00),
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

    // 3. Fallback: Sonnet 4.6
    (3.00 / 1_000_000.0, "$3.00/1M (Sonnet 4.6 default — set ANTHROPIC_MODEL or cost_per_million_tokens in ccr.toml to adjust)".to_string())
}

pub fn run(history: bool, days: u32) -> Result<()> {
    let records = load_records()?;

    if history {
        print_history(&records, days);
    } else {
        print_summary(&records);
    }

    Ok(())
}

// ─── Data loading ──────────────────────────────────────────────────────────────

fn load_records() -> Result<Vec<Analytics>> {
    let path = dirs::data_local_dir()
        .map(|d| d.join("ccr").join("analytics.jsonl"))
        .filter(|p| p.exists());

    let records = match path {
        None => vec![],
        Some(p) => {
            let content = std::fs::read_to_string(&p)?;
            content
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        }
    };
    Ok(records)
}

// ─── Summary view (default) ────────────────────────────────────────────────────

fn print_summary(records: &[Analytics]) {
    let total_input: usize = records.iter().map(|r| r.input_tokens).sum();
    let total_output: usize = records.iter().map(|r| r.output_tokens).sum();
    let total_saved = total_input.saturating_sub(total_output);
    let overall_pct = savings_pct(total_input, total_output);
    let (price_per_token, price_label) = resolve_price();
    let cost_saved = total_saved as f64 * price_per_token;

    let now_secs = now_unix();
    let today_start = day_start(now_secs);
    let week_start = now_secs.saturating_sub(7 * 86400);

    let today: Vec<&Analytics> = records
        .iter()
        .filter(|r| r.timestamp_secs >= today_start)
        .collect();
    let week: Vec<&Analytics> = records
        .iter()
        .filter(|r| r.timestamp_secs >= week_start)
        .collect();

    // ── Header ──
    println!("CCR Token Savings");
    println!("{}", "═".repeat(49));
    println!("  Runs:           {}", records.len());
    println!(
        "  Tokens saved:   {}  ({:.1}%)",
        fmt_tokens(total_saved),
        overall_pct
    );
    println!(
        "  Cost saved:     ~{}  (at {})",
        fmt_cost(cost_saved),
        price_label
    );

    if !today.is_empty() {
        let t_saved: usize = today.iter().map(|r| r.tokens_saved()).sum();
        let t_in: usize = today.iter().map(|r| r.input_tokens).sum();
        let t_out: usize = today.iter().map(|r| r.output_tokens).sum();
        println!(
            "  Today:          {} runs · {} saved · {:.1}%",
            today.len(),
            fmt_tokens(t_saved),
            savings_pct(t_in, t_out)
        );
    }
    if week.len() > today.len() {
        let w_saved: usize = week.iter().map(|r| r.tokens_saved()).sum();
        let w_in: usize = week.iter().map(|r| r.input_tokens).sum();
        let w_out: usize = week.iter().map(|r| r.output_tokens).sum();
        println!(
            "  7-day:          {} runs · {} saved · {:.1}%",
            week.len(),
            fmt_tokens(w_saved),
            savings_pct(w_in, w_out)
        );
    }

    if records.is_empty() {
        return;
    }

    // ── Per-command table ──
    println!();
    println!("Per-Command Breakdown");

    // Group: cmd -> (input, output, count, total_duration_ms, duration_count)
    let mut by_cmd: BTreeMap<String, CmdStats> = BTreeMap::new();

    for r in records {
        let key = r.command.clone().unwrap_or_else(|| "(pipeline)".to_string());
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
    // Sort by tokens saved descending
    rows.sort_by(|a, b| b.1.saved().cmp(&a.1.saved()));

    let col_w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(7).max(7);
    let sep = "─".repeat(col_w + 51);
    println!("{}", sep);
    println!(
        "{:<col_w$} {:>6}  {:>10}  {:>8}  {:>7}  {}",
        "COMMAND",
        "RUNS",
        "SAVED",
        "SAVINGS",
        "AVG ms",
        "IMPACT",
        col_w = col_w
    );
    println!("{}", sep);

    for (cmd, stats) in &rows {
        let pct = savings_pct(stats.input, stats.output);
        let avg_ms = if stats.ms_count > 0 {
            format!("{:>6}", stats.total_ms / stats.ms_count)
        } else {
            "     —".to_string()
        };
        let bar_len = (pct / 5.0) as usize;
        let bar = "█".repeat(bar_len.min(20));
        println!(
            "{:<col_w$} {:>6}  {:>10}  {:>7.1}%  {}  {}",
            cmd,
            stats.count,
            fmt_tokens(stats.saved()),
            pct,
            avg_ms,
            bar,
            col_w = col_w
        );
    }
}

// ─── History view (--history) ─────────────────────────────────────────────────

fn print_history(records: &[Analytics], days: u32) {
    let (price_per_token, _) = resolve_price();
    let now_secs = now_unix();
    let cutoff = now_secs.saturating_sub(days as u64 * 86400);

    // Group by calendar day (local date string "YYYY-MM-DD")
    let mut by_day: BTreeMap<String, DayStats> = BTreeMap::new();

    for r in records.iter().filter(|r| r.timestamp_secs >= cutoff) {
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

    println!("CCR Daily History  (last {} days)", days);
    println!("{}", "═".repeat(60));

    let sep = "─".repeat(60);
    println!("{}", sep);
    println!(
        "{:<12}  {:>5}  {:>12}  {:>8}  {:>10}",
        "DATE", "RUNS", "SAVED", "SAVINGS", "COST SAVED"
    );
    println!("{}", sep);

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
        println!(
            "{:<12}  {:>5}  {:>12}  {:>8}  {:>10}",
            day, stats.count, saved_str, pct_str, cost_str
        );
        total_input += stats.input;
        total_output += stats.output;
        total_count += stats.count;
    }

    println!("{}", sep);
    let total_saved = total_input.saturating_sub(total_output);
    let total_cost = total_saved as f64 * price_per_token;
    println!(
        "{:<12}  {:>5}  {:>12}  {:>8}  {:>10}",
        format!("{}-day total", days),
        total_count,
        fmt_tokens(total_saved),
        format!("{:.1}%", savings_pct(total_input, total_output)),
        fmt_cost(total_cost)
    );

    // Top commands over the period
    let mut cmd_stats: BTreeMap<String, CmdStats> = BTreeMap::new();
    for r in records.iter().filter(|r| r.timestamp_secs >= cutoff) {
        let key = r.command.clone().unwrap_or_else(|| "(pipeline)".to_string());
        let e = cmd_stats.entry(key).or_default();
        e.input += r.input_tokens;
        e.output += r.output_tokens;
        e.count += 1;
    }
    if !cmd_stats.is_empty() {
        let mut cmd_rows: Vec<(String, CmdStats)> = cmd_stats.into_iter().collect();
        cmd_rows.sort_by(|a, b| b.1.saved().cmp(&a.1.saved()));

        println!();
        println!("Top Commands");
        println!("{}", "─".repeat(42));
        println!("{:<14} {:>5}  {:>10}  {:>7}", "COMMAND", "RUNS", "SAVED", "SAVINGS");
        println!("{}", "─".repeat(42));
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

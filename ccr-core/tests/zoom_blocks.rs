/// Unit tests for ZI — Zoom-In block registry and marker embedding.
///
/// Tests that:
///   1. zoom disabled → no ZI_ ID in collapse/omission markers
///   2. zoom enabled → ZI_N IDs appear, original lines are registered
///   3. IDs increment correctly across multiple collapses
///   4. drain() clears the block list
///   5. Pipeline result carries zoom_blocks when zoom is enabled
use panda_core::config::{CommandConfig, FilterAction, FilterPattern, SimpleAction};
use panda_core::patterns::PatternFilter;
use panda_core::zoom;

fn make_collapse_filter() -> PatternFilter {
    PatternFilter::new(&CommandConfig {
        patterns: vec![FilterPattern {
            regex: r"^\s*Compiling \S+".to_string(),
            action: FilterAction::Simple(SimpleAction::Collapse),
            strip_ansi: false,
        }],
        on_empty: None,
    })
    .unwrap()
}

fn make_remove_filter() -> PatternFilter {
    PatternFilter::new(&CommandConfig {
        patterns: vec![FilterPattern {
            regex: r"^noise:".to_string(),
            action: FilterAction::Simple(SimpleAction::Remove),
            strip_ansi: false,
        }],
        on_empty: None,
    })
    .unwrap()
}

// ── collapse markers ──────────────────────────────────────────────────────────

#[test]
fn zoom_disabled_no_id_in_collapse_output() {
    zoom::disable();
    let filter = make_collapse_filter();
    let input = "   Compiling foo v1.0\n   Compiling bar v2.0\nerror: build failed";
    let result = filter.apply(input);
    assert!(result.contains("collapsed"), "collapse marker must still appear");
    assert!(!result.contains("ZI_"), "no zoom ID when zoom is disabled");
    assert!(!result.contains("ccr expand"), "no expand hint when disabled");
}

#[test]
fn zoom_enabled_id_in_collapse_output() {
    zoom::enable();
    let filter = make_collapse_filter();
    let input = "   Compiling foo v1.0\n   Compiling bar v2.0\nerror: build failed";
    let result = filter.apply(input);
    assert!(result.contains("ZI_"), "zoom ID must appear when zoom is enabled");
    assert!(result.contains("ccr expand"), "expand hint must appear");
    assert!(result.contains("collapsed"), "collapse count must still appear");
}

#[test]
fn zoom_blocks_contain_original_collapsed_lines() {
    zoom::enable();
    let filter = make_collapse_filter();
    let input = "   Compiling foo v1.0\n   Compiling bar v2.0\n   Compiling baz v3.0\nerror: done";
    filter.apply(input);

    let blocks = zoom::drain();
    assert!(!blocks.is_empty(), "at least one block must be registered");
    let block = &blocks[0];
    assert!(
        block.lines.iter().any(|l| l.contains("Compiling foo")),
        "block must contain original lines"
    );
    assert!(
        block.lines.iter().any(|l| l.contains("Compiling baz")),
        "all collapsed lines must be in the block"
    );
    // Error line must NOT be in the zoom block (it's not collapsed)
    assert!(
        !block.lines.iter().any(|l| l.contains("error:")),
        "non-collapsed lines must not appear in the zoom block"
    );
}

#[test]
fn zoom_ids_increment_across_multiple_collapse_groups() {
    zoom::enable();
    let filter = make_collapse_filter();

    // Two separate runs produce ZI_1 and ZI_2
    let out1 = filter.apply("   Compiling alpha v1.0\n   Compiling beta v1.0\nfirst result");
    let out2 = filter.apply("   Compiling gamma v1.0\n   Compiling delta v1.0\nsecond result");

    assert!(out1.contains("ZI_1"), "first collapse should be ZI_1, got: {}", out1);
    assert!(out2.contains("ZI_2"), "second collapse should be ZI_2, got: {}", out2);
}

#[test]
fn zoom_drain_clears_block_list() {
    zoom::enable();
    let filter = make_collapse_filter();
    filter.apply("   Compiling something v1.0\n");

    let blocks = zoom::drain();
    assert!(!blocks.is_empty(), "drain should return blocks");

    let blocks2 = zoom::drain();
    assert!(blocks2.is_empty(), "second drain must return empty list");
}

#[test]
fn remove_action_does_not_register_zoom_block() {
    zoom::enable();
    let filter = make_remove_filter();
    // Remove action should not create zoom blocks — there's nothing to expand
    filter.apply("noise: line1\nnoise: line2\nkeep this");

    let blocks = zoom::drain();
    assert!(
        blocks.is_empty(),
        "Remove action must not create zoom blocks (no content to show)"
    );
}

// ── pipeline integration ──────────────────────────────────────────────────────

#[test]
fn pipeline_result_carries_zoom_blocks_when_enabled() {
    use panda_core::config::{CcrConfig, CommandConfig};
    use panda_core::pipeline::Pipeline;
    use std::collections::HashMap;

    zoom::enable();

    // Create a config with a collapse pattern for "mytool".
    // Use "VERBOSE:" prefix instead of "Compiling" to avoid the global_rules
    // build-progress strip that would remove lines before the pattern fires.
    let mut commands = HashMap::new();
    commands.insert(
        "mytool".to_string(),
        CommandConfig {
            patterns: vec![FilterPattern {
                regex: r"^VERBOSE: loading ".to_string(),
                action: FilterAction::Simple(SimpleAction::Collapse),
                strip_ansi: false,
            }],
            on_empty: None,
        },
    );
    let config = CcrConfig { commands, ..CcrConfig::default() };
    let pipeline = Pipeline::new(config);

    let input = (0..5)
        .map(|i| format!("VERBOSE: loading module{}", i))
        .chain(std::iter::once("error[E0001]: something broke".to_string()))
        .collect::<Vec<_>>()
        .join("\n");

    let result = pipeline.process(&input, Some("mytool"), None, None).unwrap();

    assert!(
        !result.zoom_blocks.is_empty(),
        "PipelineResult must carry zoom blocks when zoom is enabled"
    );
    assert!(
        result.output.contains("ZI_"),
        "pipeline output must embed zoom ID"
    );
    assert!(
        result.output.contains("error[E0001]"),
        "critical lines must still appear in output"
    );
}

#[test]
fn pipeline_result_empty_zoom_blocks_when_disabled() {
    use panda_core::pipeline::Pipeline;

    zoom::disable();

    let input = (0..5)
        .map(|i| format!("   Compiling crate{} v1.0", i))
        .chain(std::iter::once("done".to_string()))
        .collect::<Vec<_>>()
        .join("\n");

    let result = Pipeline::new(CcrConfig::default())
        .process(&input, None, None, None)
        .unwrap();

    assert!(
        result.zoom_blocks.is_empty(),
        "zoom_blocks must be empty when zoom is disabled"
    );
}

use panda_core::config::CcrConfig;

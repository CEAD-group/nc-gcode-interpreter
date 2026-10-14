#![cfg(feature = "bench-experiments")]
#[path = "../benches/fixtures.rs"]
mod fixtures;
use nc_gcode_interpreter::experiments::*;

fn reference(source: &str) -> String {
    observe(source, None, |output, state| {
        GrammarCache::new(source)
            .unwrap()
            .execute(output, state)
            .map_err(|e| e.to_string())
    })
    .unwrap()
}
fn cached(cache: &DecodedCache<'_>, source: &str) -> String {
    observe(source, None, |output, state| {
        cache.execute(output, state).map_err(|e| e.to_string())
    })
    .unwrap()
}
#[test]
fn strategies_match_grammar_including_events_provenance_and_final_state() {
    for source in [
        fixtures::program("flat", 200),
        "MULTIPLIER=2\nG1 X0 E0\n\n; café\nX=IC(2) E=IC(MULTIPLIER*0.5)\nMULTIPLIER=3\nX2\nM30\nX999\n".into(),
    ] {
        for size in [1, 7, 64, 256] {
            let cache = DecodedCache::new(&source, size).unwrap();
            assert_eq!(cached(&cache, &source), reference(&source));
        }
        for strategy in [bounded_decode, fused_decode] {
            let observed = observe(&source, None, |output, state| strategy(&source, output, state)).unwrap();
            assert_eq!(observed, reference(&source));
        }
    }
}
#[test]
fn edit_matrix_and_undo_match_fresh_execution() {
    let source = fixtures::program("flat", 80);
    for size in [1, 7, 64, 256] {
        for edit in ["coordinate", "parameter", "insert", "delete"] {
            let (revised, range, replacement) = fixtures::edit(&source, edit, true);
            let original = source
                .lines()
                .skip(range.start)
                .take(range.len())
                .collect::<Vec<_>>()
                .join("\n");
            let mut cache = DecodedCache::new(&source, size).unwrap();
            cache.replace(range.clone(), &replacement).unwrap();
            assert_eq!(cached(&cache, &revised), reference(&revised), "{size} {edit}");
            cache
                .replace(range.start..range.start + replacement.lines().count(), &original)
                .unwrap();
            assert_eq!(cached(&cache, &source), reference(&source));
        }
    }
}
#[test]
fn randomized_edit_sequences_keep_line_numbers_and_state() {
    // Reproducible edits across boundaries, empty files, inserts, deletions,
    // parameter changes and M30. Arena retains immutable revisions for borrows.
    let mut seed = 42_u64;
    let mut source = fixtures::program("flat", 32);
    let mut edits = Vec::new();
    for _ in 0..100 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let count = source.lines().count();
        let start = (seed as usize >> 8) % (count + 1);
        let end = (start + (seed as usize % 3)).min(count);
        let replacement = match seed % 4 {
            0 => "",
            1 => "; inserted\n",
            2 => "G1 X17 Y3\n",
            _ => "MULTIPLIER=2\nG1 E0\n",
        }
        .to_owned();
        let mut lines: Vec<_> = source.lines().collect();
        lines.splice(start..end, replacement.lines());
        source = lines.join("\n");
        if !source.is_empty() {
            source.push('\n');
        }
        edits.push((start..end, replacement, source.clone()));
    }
    let initial = fixtures::program("flat", 32);
    for size in [1, 8, 64] {
        let mut cache = DecodedCache::new(&initial, size).unwrap();
        for (range, text, revised) in &edits {
            cache.replace(range.clone(), text).unwrap();
            assert_eq!(cached(&cache, revised), reference(revised));
        }
    }
}
#[test]
fn unsupported_or_invalid_edits_are_atomic() {
    let source = fixtures::program("flat", 40);
    let mut cache = DecodedCache::new(&source, 8).unwrap();
    for replacement in ["TRANS X1", "IF 1==1", "LABEL: X1", "X=", "GOTOF TARGET"] {
        assert!(cache.replace(3..4, replacement).is_err());
        assert_eq!(cached(&cache, &source), reference(&source));
    }
    assert!(cache.replace(41..42, "X1").is_err());
    for range in [0..0, 0..40, 39..40, 40..40] {
        let mut cache = DecodedCache::new(&source, 8).unwrap();
        let mut lines: Vec<_> = source.lines().collect();
        lines.splice(range.clone(), ["X3"]);
        let revised = lines.join("\n");
        cache.replace(range, "X3").unwrap();
        assert_eq!(cached(&cache, &revised), reference(&revised));
    }
}

#[test]
fn flattened_arcs_and_splines_match_with_cached_execution() {
    for source in [
        "G1 X0 Y0
G2 X10 Y0 I5 J0
G1 X10 Y5
",
        "G1 X0 Y0
BSPLINE
X1 Y2
X2 Y3
X3 Y0
G1
",
    ] {
        let cache = DecodedCache::new(source, 2).unwrap();
        let expected = observe(source, Some(0.05), |out, state| {
            GrammarCache::new(source)
                .unwrap()
                .execute(out, state)
                .map_err(|e| e.to_string())
        })
        .unwrap();
        let actual = observe(source, Some(0.05), |out, state| {
            cache.execute(out, state).map_err(|e| e.to_string())
        })
        .unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn errors_retain_current_source_location_after_insertion() {
    let source = "X0
X=IC(MISSING*0.5)
";
    let revised = "; inserted\nX0\nX=IC(MISSING*0.5)\n";
    let mut cache = DecodedCache::new(source, 1).unwrap();
    cache.replace(0..0, "; inserted").unwrap();
    let actual = observe(revised, None, |out, state| {
        cache.execute(out, state).map_err(|e| e.to_string())
    })
    .unwrap_err();
    let expected = observe(revised, None, |out, state| {
        GrammarCache::new(revised)
            .unwrap()
            .execute(out, state)
            .map_err(|e| e.to_string())
    })
    .unwrap_err();
    assert_eq!(actual, expected);
}

#[test]
fn benchmark_fixtures_match_public_streaming_api() {
    use nc_gcode_interpreter::interpreter::nc_to_row_stream;
    for shape in ["flat", "calls", "grammar", "structured", "frames"] {
        let source = fixtures::program(shape, 2100);
        let expected = reference(&source);
        let actual = std::thread::scope(|scope| {
            let (tx, rx) = std::sync::mpsc::sync_channel(32);
            let reader = scope.spawn(move || rx.into_iter().collect::<Vec<_>>());
            let state = nc_to_row_stream(
                &source,
                None,
                None,
                Some(vec!["ELX".into()]),
                10_000,
                None,
                false,
                None,
                tx,
            )
            .unwrap();
            observation(&reader.join().unwrap(), &state)
        });
        assert_eq!(actual, expected, "{shape}");
    }
}

#[test]
fn local_edits_decode_bounded_neighbourhoods() {
    let source = fixtures::program("flat", 10_000);
    for size in [64, 1024] {
        let mut cache = DecodedCache::new(&source, size).unwrap();
        cache.replace(5000..5001, "X9").unwrap();
        assert!(cache.decoded_lines <= 2 * size + 1);
    }
}

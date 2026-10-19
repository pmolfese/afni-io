//! AFNI-source snapshots and README examples for the DRIVE_AFNI language.
#![cfg(feature = "talk")]

use std::collections::HashSet;

use afni_io::drive_afni::{
    parse_with, Command, ParseOptions, Recognition, Registry, STATIC_COMMANDS,
};

#[test]
fn static_table_matches_afni_source_order() {
    let expected: Vec<_> = include_str!("data/talk/drive_afni_static_commands.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let actual: Vec<_> = STATIC_COMMANDS.iter().map(|spec| spec.name).collect();

    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 101);
    assert_eq!(actual.iter().copied().collect::<HashSet<_>>().len(), 100);
    assert_eq!(
        actual
            .iter()
            .filter(|&&name| name == "WRITE_UNDERLAY")
            .count(),
        2
    );
}

#[test]
fn readme_examples_parse_to_their_canonical_commands_losslessly() {
    let registry = Registry::with_documented_plugins();
    for (line_number, line) in include_str!("data/talk/drive_afni_examples.txt")
        .lines()
        .enumerate()
    {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (expected, source) = line
            .split_once('\t')
            .unwrap_or_else(|| panic!("fixture line {} has no tab", line_number + 1));
        let parsed = parse_with(source, &registry, ParseOptions::default())
            .unwrap_or_else(|error| panic!("fixture line {}: {error}", line_number + 1));
        assert_eq!(
            parsed.wire_command(),
            source,
            "fixture line {}",
            line_number + 1
        );
        let Command::Known(command) = parsed.command else {
            panic!("fixture line {} parsed as raw", line_number + 1);
        };
        assert_eq!(
            command.canonical_name(),
            expected,
            "fixture line {}",
            line_number + 1
        );
        if expected == "DATASET#N" {
            assert!(matches!(command.recognition, Recognition::Dynamic(_)));
        }
    }
}

#[test]
fn every_alias_resolves_to_a_static_canonical_label() {
    for alias in STATIC_COMMANDS.iter().filter(|spec| spec.is_alias()) {
        assert!(
            STATIC_COMMANDS
                .iter()
                .any(|candidate| candidate.name == alias.canonical),
            "{} aliases missing canonical label {}",
            alias.name,
            alias.canonical
        );
    }
}

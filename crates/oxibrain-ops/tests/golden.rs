//! Integration-level guard: the registry's `tools/list` output is
//! byte-identical to the blessed pre-refactor fixture (ADR-012).

#[test]
fn tools_list_matches_blessed_fixture() {
    let generated = serde_json::to_string_pretty(&oxibrain_ops::tools_list()).unwrap();
    let fixture = include_str!("golden_tools_list.json");
    assert_eq!(generated.trim(), fixture.trim());
}

//! Server-layer regression tests: Host guard and export-boundary hardening.
//! (Full HTTP wiring is validated live; these lock in the security invariants.)

use cursordump::export::validate_out_dir;

#[test]
fn export_refuses_inside_cursor_and_populated_dirs() {
    let home = dirs::home_dir().unwrap();
    let cursor = home.join(".cursor");

    // Inside ~/.cursor — refused (containment canonicalizes the deepest
    // existing ancestor, so this only applies where ~/.cursor exists; CI has none).
    if cursor.is_dir() {
        assert!(validate_out_dir(&cursor.join("projects").join("x"), &cursor).is_err());
    }

    // Fresh temp dir — allowed.
    let fresh = std::env::temp_dir().join("cursordump-srvtest-fresh");
    let _ = std::fs::remove_dir_all(&fresh);
    assert!(validate_out_dir(&fresh, &cursor).is_ok());

    // Populated non-dump dir — refused (won't clobber user files).
    let pop = std::env::temp_dir().join("cursordump-srvtest-pop");
    let _ = std::fs::remove_dir_all(&pop);
    std::fs::create_dir_all(&pop).unwrap();
    std::fs::write(pop.join("keep.txt"), "x").unwrap();
    assert!(validate_out_dir(&pop, &cursor).is_err());

    // Same dir once it looks like a prior dump — allowed (re-export).
    std::fs::write(pop.join("manifest.json"), "{}").unwrap();
    assert!(validate_out_dir(&pop, &cursor).is_ok());
    let _ = std::fs::remove_dir_all(&pop);
}

//! Headless search over real Cursor data when available (read-only).

use std::sync::mpsc::channel;

use cursordump::scanner;
use cursordump::search::{run_search, SearchCancel, SearchEvent};

#[test]
fn search_streams_hits_and_finishes() {
    let Some(root) = scanner::default_root().filter(|r| r.is_dir()) else {
        eprintln!("no ~/.cursor/projects — skipping");
        return;
    };
    let projects = scanner::scan_projects(&root);
    if projects.iter().all(|p| p.sessions.is_empty()) {
        eprintln!("no sessions — skipping");
        return;
    }

    let (tx, rx) = channel();
    // "the" is guaranteed to appear somewhere in any real corpus.
    run_search(&projects, "the", tx, SearchCancel::new(), || {});

    let mut hits = 0usize;
    let mut finished = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            SearchEvent::Hit(hit) => {
                hits += 1;
                assert!(!hit.snippet.is_empty());
                assert!(hit.session.path.is_file());
            }
            SearchEvent::Finished { files_scanned } => {
                assert!(files_scanned > 0);
                finished = true;
            }
        }
    }
    assert!(finished, "search must send Finished");
    assert!(hits > 0, "expected at least one hit");
}

#[test]
fn cancelled_search_stops() {
    let Some(root) = scanner::default_root().filter(|r| r.is_dir()) else {
        return;
    };
    let projects = scanner::scan_projects(&root);
    let cancel = SearchCancel::new();
    cancel.cancel();
    let (tx, rx) = channel();
    run_search(&projects, "the", tx, cancel, || {});
    let mut hits = 0;
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, SearchEvent::Hit(_)) {
            hits += 1;
        }
    }
    assert_eq!(hits, 0, "cancelled search must not produce hits");
}

use anyhow::Result;
use camino::Utf8PathBuf;
use codegraph_extract::Orchestrator;
use codegraph_graph::GraphIndex;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebouncedEvent};
use std::collections::BTreeSet;
use std::time::Duration;

/// Spawn a debounced watcher that full re-indexes the workspace on file changes.
/// Runs on a background tokio task; cancellation when the runtime drops.
/// `dsn = None` (in-memory backend) → không có file ngoài để theo dõi, bỏ qua.

fn run(root: Utf8PathBuf, dsn: String) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<DebouncedEvent>>();
    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        None,
        move |res: notify_debouncer_full::DebounceEventResult| {
            if let Ok(events) = res {
                let _ = tx.send(events);
            }
        },
    )?;
    debouncer.watch(root.as_std_path(), RecursiveMode::Recursive)?;

    let ignored_dirs = [codegraph_extract::project_dir(&root), root.join(".git")];
    let mut gitignore_builder = GitignoreBuilder::new(root.as_std_path());
    gitignore_builder.add(root.join(".gitignore"));
    let gitignore = gitignore_builder.build().unwrap_or_else(|_| {
        GitignoreBuilder::new(root.as_std_path())
            .build()
            .expect("empty gitignore builder must build")
    });

    let orch = Orchestrator::with_registry();
    let handle = tokio::runtime::Handle::current();
    while let Ok(events) = rx.recv() {
        let mut batch = events;
        // Coalesce any batches that arrive while we're about to process one -
        // avoids back-to-back re-indexes when the debouncer fires repeatedly
        // in quick succession (e.g. during a large rescan).
        while let Ok(more) = rx.try_recv() {
            batch.extend(more);
        }

        let paths = relevant_paths(&batch, &root, &ignored_dirs, &gitignore);
        if paths.is_empty() {
            continue;
        }
        // Full re-index (đã chốt — bỏ incremental): bất kỳ thay đổi nào cũng
        // index lại toàn bộ (ingest reset + rebuild engine).
        let result = handle.block_on(async {
            let mut idx = GraphIndex::open(&dsn).await?;
            orch.index_all(&root, &mut idx, None).await
        });
        match result {
            Ok(s) if s.files > 0 => tracing::info!(
                "watch re-index: {} files, {} symbols, {} chains, {} calls",
                s.files,
                s.symbols,
                s.chains,
                s.calls
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!("re-index failed: {e}"),
        }
    }
    Ok(())
}

fn relevant_paths(
    events: &[DebouncedEvent],
    root: &Utf8PathBuf,
    ignored_dirs: &[Utf8PathBuf],
    gitignore: &Gitignore,
) -> Vec<Utf8PathBuf> {
    let mut out = BTreeSet::new();
    for event in events {
        if event.need_rescan() {
            continue;
        }
        for p in &event.paths {
            if ignored_dirs
                .iter()
                .any(|dir| p.starts_with(dir.as_std_path()))
            {
                continue;
            }
            if gitignore.matched(p, p.is_dir()).is_ignore() {
                continue;
            }
            let Ok(p) = Utf8PathBuf::from_path_buf(p.clone()) else {
                continue;
            };
            if p.starts_with(root) {
                out.insert(p);
            }
        }
    }
    out.into_iter().collect()
}

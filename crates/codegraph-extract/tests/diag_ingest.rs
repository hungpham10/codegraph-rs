//! TEMPORARY diagnostic: does the full ingest pipeline (parse → GraphIndex::ingest)
//! preserve the chain of the root `func main` in terraform/main.go?
//!
//! The live DB (terraform/.codegraph/db.sqlite) has rt_chains with records only
//! 102..=14006 while symbols go to 72150 — the root main (id 72105) lost its chain.
//! This test ingests just main.go into a fresh index and reports what survives.

use codegraph_extract::registry;
use codegraph_graph::GraphIndex;

const MAIN_GO: &str = "/Users/lap02921/Desktop/Workspace/terraform/main.go";

#[tokio::test]
async fn diag_ingest_terraform_root_main() {
    let src = std::fs::read_to_string(MAIN_GO).expect("read main.go");
    let parser = registry()
        .into_iter()
        .find(|p| p.name() == "go")
        .expect("go parser");
    let res = parser.parse_file("main.go", &src).expect("parse");

    // What does the extractor produce?
    let main_sym = res
        .symbols
        .iter()
        .find(|s| s.name == "main" && s.kind == codegraph_core::SymbolKind::Function)
        .expect("func main symbol")
        .clone();
    let main_chain = res
        .chains
        .get(&main_sym.id)
        .expect("extractor produced chain for main");
    println!(
        "extractor: main id={} line={} chain_len={}",
        main_sym.id,
        main_sym.line,
        main_chain.len()
    );
    println!("extractor: chains produced for {} funcs", res.chains.len());
    let main_id = main_sym.id;

    // Ingest into a fresh index (single file → ids keep their local values).
    let dir = tempfile::tempdir().expect("tempdir");
    let mut idx = GraphIndex::open(dir.path().join("db.sqlite").to_str().unwrap())
        .await
        .expect("open index");
    idx.ingest(&[res]).await.expect("ingest");

    // After single-file ingest the id is unchanged (SYMBOL_BASE identity remap).
    let callees = idx.callees(main_id).await.expect("callees");
    println!(
        "ingested: main({}) callees = {:?}",
        main_sym.id,
        callees.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
    );

    let flow = idx.flow(main_id).await;
    println!("ingested: flow(main) = {:?}", flow.is_ok());

    // Drop the index and reopen from disk — what survived persistence?
    drop(idx);
    let idx2 = GraphIndex::open(dir.path().join("db.sqlite").to_str().unwrap())
        .await
        .expect("reopen");
    let callees2 = idx2.callees(main_id).await.expect("callees after reopen");
    println!(
        "reopened: main({}) callees = {:?}",
        main_sym.id,
        callees2.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
    );
    assert!(
        !callees2.is_empty(),
        "root main lost its chain through ingest+persist"
    );
}

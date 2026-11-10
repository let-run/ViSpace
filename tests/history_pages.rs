use vispace::action_log::{self, Journal, Record};
use std::io::{Seek, SeekFrom, Write};
fn record(index: u64) -> Record {
    Record {
        version: 1,
        id: vispace::model::id(),
        timestamp_ms: index,
        initial: index == 0,
        actions: vec![],
        mesh: None,
        proof: None,
    }
}
#[test]
fn pages_are_newest_first_and_gapless_across_chunks_and_new_appends() {
    let temp = tempfile::tempdir().unwrap();
    let mut journal = Journal::open(temp.path().join("actions")).unwrap();
    let mut ids = vec![];
    for i in 0..421 {
        if i % 150 == 0 {
            journal.start_chunk().unwrap();
        }
        let record = record(i);
        ids.push(record.id.clone());
        journal.append(&record).unwrap();
    }
    journal.start_chunk().unwrap(); // Empty writer chunks are not history entries.
    let first = journal.page(None, 200).unwrap();
    assert_eq!(first.entries.len(), 200);
    assert_eq!(first.entries[0].entry.record.id, *ids.last().unwrap());
    for i in 421..434 {
        journal.append(&record(i)).unwrap();
    }
    let second = journal.page(first.next.as_ref(), 200).unwrap();
    assert_eq!(second.entries.len(), 200);
    let third = journal.page(second.next.as_ref(), 200).unwrap();
    assert_eq!(third.entries.len(), 21);
    assert!(third.next.is_none());
    let actual: Vec<_> = first
        .entries
        .iter()
        .chain(&second.entries)
        .chain(&third.entries)
        .map(|e| e.entry.record.id.clone())
        .collect();
    assert_eq!(actual, ids.into_iter().rev().collect::<Vec<_>>());
    let fresh = journal.page(None, 200).unwrap();
    assert_eq!(fresh.entries[0].entry.record.timestamp_ms, 433);
    assert!(journal.page(None, 0).is_err());
    assert!(journal.page(None, 201).is_err());
}
#[test]
fn exact_pages_detect_the_end_and_ignore_an_incomplete_tail() {
    let temp = tempfile::tempdir().unwrap();
    let mut journal = Journal::open(temp.path().join("actions")).unwrap();
    let mut name = String::new();
    for i in 0..200 {
        name = journal.append(&record(i)).unwrap().0;
    }
    let path = temp.path().join("actions").join(name);
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&[100, 0, 0, 0, 31])
        .unwrap();
    let page = journal.page(None, 200).unwrap();
    assert_eq!(page.entries.len(), 200);
    assert!(page.next.is_none());
}
#[test]
fn older_payloads_are_not_decompressed_until_requested_and_hashes_fingerprint_record_contents() {
    let temp = tempfile::tempdir().unwrap();
    let mut journal = Journal::open(temp.path().join("actions")).unwrap();
    let mut name = String::new();
    for i in 0..201 {
        name = journal.append(&record(i)).unwrap().0;
    }
    let path = temp.path().join("actions").join(name);
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(12)).unwrap();
    file.write_all(&[0]).unwrap(); // Corrupt only the oldest gzip member.
    let page = journal.page(None, 200).unwrap();
    assert_eq!(page.entries.len(), 200);
    assert!(journal.page(page.next.as_ref(), 200).is_err());
    let item = &page.entries[0];
    let roundtrip: Record =
        action_log::decode_frame(&action_log::encode_frame(&item.entry.record).unwrap()).unwrap();
    assert_eq!(item.hash, action_log::record_hash(&roundtrip).unwrap());
    assert_eq!(item.hash.len(), 40);
    let mut changed = roundtrip;
    changed.timestamp_ms += 1;
    assert_ne!(item.hash, action_log::record_hash(&changed).unwrap());
}

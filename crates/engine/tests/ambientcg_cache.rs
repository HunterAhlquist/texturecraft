//! The ambientCG download cache (its own test binary: the cache folder is process-wide).

use std::sync::{Arc, Mutex};

use texturecraft_engine::ambientcg_cmds::{Fetched, download, install_cache, install_fetcher};
use texturecraft_engine::formats::ambientcg::Download;

static GETS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn d(id: &str, size: u64) -> Download {
    Download { attributes: "1K-JPG".into(), url: format!("https://ambientcg.com/get?file={id}_1K-JPG.zip"), size }
}

#[test]
fn downloads_are_cached_by_version_and_pruned_to_the_cap() {
    let dir = std::env::temp_dir().join(format!("tc_acg_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    install_cache(dir.clone(), 25);
    install_fetcher(Arc::new(|url: &str, _max: u64, _head: bool| {
        GETS.lock().unwrap().push(url.to_string());
        // The body is as long as the file name says (`…_<len>…`): 10 bytes for A, 12 for B.
        let len = if url.contains("A_") { 10 } else { 12 };
        Ok(Fetched { bytes: vec![7; len], sha1: Some("ABCD".into()) })
    }));
    let gets = |url: &str| GETS.lock().unwrap().iter().filter(|u| *u == url).count();

    let a = d("A", 10);
    let first = download("A", &a).unwrap();
    assert_eq!((first.bytes.len(), first.sha1.as_deref()), (10, Some("abcd")), "the SHA-1 is kept lowercase");
    let again = download("A", &a).unwrap();
    assert_eq!(again.bytes, first.bytes);
    assert_eq!(again.sha1.as_deref(), Some("abcd"), "the cache keeps the SHA-1");
    assert_eq!(gets(&a.url), 1, "the second one comes from the cache");

    // Another size on ambientCG is another version: downloaded again (and not cached, since the
    // body doesn't match the advertised size).
    let a2 = d("A", 11);
    download("A", &a2).unwrap();
    assert_eq!(gets(&a.url), 2);

    // A second asset pushes the folder over its 25-byte cap: the least recently used goes.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let b = d("B", 12);
    download("B", &b).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    download("A", &a).unwrap(); // a hit: A is now the most recently used
    assert_eq!(gets(&a.url), 2);
    let c = Download { attributes: "2K-JPG".into(), url: "https://ambientcg.com/get?file=B_2K-JPG.zip".into(), size: 12 };
    std::thread::sleep(std::time::Duration::from_millis(20));
    download("B", &c).unwrap();
    let zips: Vec<String> =
        std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.ends_with(".zip")).collect();
    assert!(zips.contains(&"A_1K-JPG.zip".to_string()) && zips.contains(&"B_2K-JPG.zip".to_string()), "{zips:?}");
    assert!(!zips.contains(&"B_1K-JPG.zip".to_string()), "the least recently used was dropped: {zips:?}");

    // Downloads over the limit are refused before any request.
    let huge = d("H", u64::MAX);
    assert!(download("H", &huge).is_err());
    assert_eq!(gets(&huge.url), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

//! A vector computed for a file must not land on another file that took its
//! row id while the batch was being embedded (an index rebuilt mid-pass).
//! Its own binary: the embedder is process-wide.

use lumen_core::index::NewFile;
use lumen_core::semantic::Model;
use lumen_core::*;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

fn file(path: &str) -> NewFile {
    NewFile {
        path: path.into(),
        name: path.rsplit('/').next().unwrap().into(),
        name_py: String::new(),
        parent: "/h".into(),
        ext: "pdf".into(),
        kind: Kind::Pdf,
        size: 10,
        mtime: 1000,
        ctime: 1000,
        cloud: false,
    }
}

struct Resetting(Mutex<Option<PathBuf>>);

impl Embedder for Resetting {
    fn embed(&self, items: &[(Model, String)]) -> anyhow::Result<Vec<Option<Vec<f32>>>> {
        if let Some(db) = self.0.lock().unwrap().take() {
            let other = Index::open(db).unwrap();
            other.reset().unwrap();
            other.insert_files(&[file("/h/someone-else.pdf")]).unwrap();
        }
        Ok(items.iter().map(|_| Some(vec![1.0, 0.0, 0.0])).collect())
    }
}

#[test]
fn vector_follows_its_file_not_its_old_id() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("i.db");
    let index = Index::open(&db).unwrap();
    index.insert_files(&[file("/h/mine.pdf")]).unwrap();
    set_embedder(Some(Arc::new(Resetting(Mutex::new(Some(db))))));
    let r = index_semantic(&index, Arc::new(AtomicBool::new(false)), |_| {});
    set_embedder(None);
    r.unwrap();
    let n: i64 = index.conn().query_row("SELECT count(*) FROM vectors", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "the old file's vector was stored under the new file's id");
}

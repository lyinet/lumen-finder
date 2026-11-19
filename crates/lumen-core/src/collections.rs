//! Smart collections: files that gather themselves by what they are, not where
//! they live. Each collection is a fixed rule over the name and, for documents,
//! PDFs and images, the words inside them (OCR text included).

use crate::index::{FileRecord, Index};
use crate::kind::Kind;
use anyhow::Result;
use rusqlite::types::Value;
use serde::Serialize;

pub struct Rule {
    pub id: &'static str,
    kinds: &'static [Kind],
    /// SQL LIKE patterns over the file name (ASCII is case-insensitive).
    names: &'static [&'static str],
    /// Phrases inside the text; any one is enough.
    phrases: &'static [&'static str],
}

use Kind::*;

pub const RULES: &[Rule] = &[
    Rule {
        id: "invoices",
        kinds: &[Pdf, Image, Document, Spreadsheet],
        names: &["%发票%", "%invoice%", "%receipt%", "%收据%", "%账单%", "%報銷%", "%报销%", "%領収書%"],
        phrases: &["发票代码", "发票号码", "价税合计", "電子發票", "invoice number", "invoice no", "amount due", "total due"],
    },
    Rule {
        id: "contracts",
        kinds: &[Pdf, Document, Image],
        names: &["%合同%", "%合約%", "%协议%", "%協議%", "%契約%", "%contract%", "%agreement%", "NDA%", "% NDA%", "%-NDA%"],
        phrases: &["本合同", "本合約", "甲方", "乙方", "双方协商", "this agreement", "hereinafter referred"],
    },
    Rule {
        id: "ids",
        kinds: &[Image, Pdf],
        names: &["%证件%", "%身份证%", "%身分證%", "%身份證%", "%护照%", "%護照%", "%驾照%", "%驾驶证%", "%户口%", "%passport%", "%id card%", "%id_card%", "%idcard%", "%visa%", "%签证%"],
        phrases: &["居民身份证", "國民身分證", "公民身份号码", "passport no", "中华人民共和国护照", "机动车驾驶证"],
    },
    Rule {
        id: "screenshots",
        kinds: &[Image],
        names: &["Screenshot%", "Screen Shot%", "截屏%", "屏幕快照%", "截图%", "CleanShot%", "スクリーンショット%"],
        phrases: &[],
    },
    Rule {
        id: "resumes",
        kinds: &[Pdf, Document],
        names: &["%简历%", "%簡歷%", "%resume%", "%résumé%", "CV%", "% CV%", "%-CV%", "%履歴書%"],
        phrases: &["工作经历", "教育背景", "work experience"],
    },
];

#[derive(Debug, Clone, Serialize)]
pub struct CollectionStat {
    pub id: &'static str,
    pub files: i64,
    pub bytes: i64,
    pub latest: i64,
}

impl Rule {
    fn filter(&self) -> (String, Vec<Value>) {
        let kinds = self.kinds.iter().map(|k| k.as_i64().to_string()).collect::<Vec<_>>().join(",");
        let mut ors: Vec<String> = Vec::new();
        let mut args: Vec<Value> = Vec::new();
        for n in self.names {
            ors.push("f.name LIKE ?".into());
            args.push(Value::Text((*n).into()));
        }
        let expr = self
            .phrases
            .iter()
            .filter_map(|p| crate::cjk::match_expr(p))
            .collect::<Vec<_>>()
            .join(" OR ");
        if !expr.is_empty() {
            ors.push("f.id IN (SELECT rowid FROM content_fts WHERE content_fts MATCH ?)".into());
            args.push(Value::Text(expr));
        }
        (format!("f.kind IN ({kinds}) AND ({})", ors.join(" OR ")), args)
    }
}

pub fn rule(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.id == id)
}

impl Index {
    /// How many files each collection holds right now.
    pub fn collections(&self) -> Result<Vec<CollectionStat>> {
        self.memo("collections", |conn| {
            let mut out = Vec::with_capacity(RULES.len());
            for r in RULES {
                let (w, args) = r.filter();
                let (files, bytes, latest) = conn.query_row(
                    &format!("SELECT count(*), coalesce(sum(f.size),0), coalesce(max(f.mtime),0) FROM files f WHERE {w}"),
                    rusqlite::params_from_iter(args),
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                out.push(CollectionStat { id: r.id, files, bytes, latest });
            }
            Ok(out)
        })
    }

    /// One collection's files, newest first.
    pub fn collection(&self, id: &str, limit: usize, offset: usize) -> Result<Vec<FileRecord>> {
        let Some(r) = rule(id) else { return Ok(Vec::new()) };
        let (w, mut args) = r.filter();
        args.push(Value::Integer(crate::index::sql_int(limit)));
        args.push(Value::Integer(crate::index::sql_int(offset)));
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM files f WHERE {w} ORDER BY f.mtime DESC LIMIT ? OFFSET ?",
            FileRecord::COLS
        ))?;
        let v = stmt
            .query_map(rusqlite::params_from_iter(args), FileRecord::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(v)
    }
}

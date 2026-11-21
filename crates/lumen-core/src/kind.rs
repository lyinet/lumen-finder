//! File classification. Lumen only indexes things a person made, saved or
//! collected: documents, music, video, images, ebooks and archives.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Document,
    Spreadsheet,
    Presentation,
    Pdf,
    Ebook,
    Audio,
    Video,
    Image,
    Archive,
    Design,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Document,
        Kind::Spreadsheet,
        Kind::Presentation,
        Kind::Pdf,
        Kind::Ebook,
        Kind::Audio,
        Kind::Video,
        Kind::Image,
        Kind::Archive,
        Kind::Design,
    ];

    pub fn as_i64(self) -> i64 {
        self as i64
    }

    pub fn from_i64(v: i64) -> Option<Kind> {
        Kind::ALL.get(v as usize).copied()
    }

    pub fn slug(self) -> &'static str {
        match self {
            Kind::Document => "document",
            Kind::Spreadsheet => "spreadsheet",
            Kind::Presentation => "presentation",
            Kind::Pdf => "pdf",
            Kind::Ebook => "ebook",
            Kind::Audio => "audio",
            Kind::Video => "video",
            Kind::Image => "image",
            Kind::Archive => "archive",
            Kind::Design => "design",
        }
    }

    /// Accepts slugs and friendly aliases (English and Chinese) used in queries.
    pub fn parse(s: &str) -> Option<Kind> {
        let s = s.to_lowercase();
        Some(match s.as_str() {
            "document" | "doc" | "docs" | "text" | "文档" | "文件" => Kind::Document,
            "spreadsheet" | "sheet" | "xls" | "表格" | "試算表" => Kind::Spreadsheet,
            "presentation" | "slides" | "ppt" | "keynote" | "幻灯片" | "演示" | "簡報" | "投影片" => Kind::Presentation,
            "pdf" => Kind::Pdf,
            "ebook" | "book" | "books" | "电子书" | "书" | "電子書" | "書" => Kind::Ebook,
            "audio" | "music" | "song" | "songs" | "音乐" | "音频" | "音樂" | "音訊" => Kind::Audio,
            "video" | "videos" | "movie" | "movies" | "视频" | "电影" | "影片" | "視頻" | "電影" => Kind::Video,
            "image" | "images" | "photo" | "photos" | "picture" | "图片" | "照片" | "圖片" | "相片" => Kind::Image,
            "archive" | "zip" | "压缩包" | "壓縮包" | "壓縮檔" => Kind::Archive,
            "design" | "设计" | "設計" => Kind::Design,
            _ => return None,
        })
    }

    pub fn from_ext(ext: &str) -> Option<Kind> {
        let e = ext.to_ascii_lowercase();
        Some(match e.as_str() {
            "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "markdown" | "pages" | "tex" | "wps"
            | "org" | "rst" | "html" | "htm" | "xhtml" => Kind::Document,
            "xls" | "xlsx" | "xlsm" | "csv" | "tsv" | "numbers" | "ods" | "et" => Kind::Spreadsheet,
            "ppt" | "pptx" | "key" | "odp" | "dps" => Kind::Presentation,
            "pdf" => Kind::Pdf,
            "epub" | "mobi" | "azw" | "azw3" | "djvu" | "fb2" | "cbz" | "cbr" => Kind::Ebook,
            "mp3" | "flac" | "wav" | "aac" | "m4a" | "aiff" | "aif" | "ogg" | "opus" | "wma"
            | "alac" | "ape" | "dsf" | "mid" | "midi" => Kind::Audio,
            "mp4" | "mov" | "mkv" | "avi" | "m4v" | "webm" | "wmv" | "flv" | "mpg" | "mpeg"
            | "3gp" | "ts" | "rmvb" | "vob" => Kind::Video,
            "jpg" | "jpeg" | "png" | "heic" | "heif" | "gif" | "webp" | "tiff" | "tif" | "bmp"
            | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "raf" | "svg" => Kind::Image,
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "dmg" | "iso" => {
                Kind::Archive
            }
            "psd" | "ai" | "sketch" | "fig" | "xd" | "indd" | "afdesign" | "afphoto" | "procreate"
            | "blend" | "c4d" => Kind::Design,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_aliases() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_i64(k.as_i64()), Some(k));
            assert_eq!(Kind::parse(k.slug()), Some(k));
        }
        assert_eq!(Kind::parse("音乐"), Some(Kind::Audio));
        assert_eq!(Kind::from_ext("MP4"), Some(Kind::Video));
        assert_eq!(Kind::from_ext("dylib"), None);
        // Saved web pages: the README promises their words are searchable.
        for e in ["html", "HTM", "xhtml"] {
            assert_eq!(Kind::from_ext(e), Some(Kind::Document), "{e}");
            assert!(crate::content::EXTS.contains(&e.to_ascii_lowercase().as_str()), "{e}");
        }
    }
}

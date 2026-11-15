// lumen-ocr: print the text Apple Vision reads in an image or a scanned PDF,
// and (since v0.6) turn sentences into vectors for semantic search.
//
//   lumen-ocr <file>          text on stdout, nothing when there is none
//   lumen-ocr embed           stdin: one "zh\t<text>" or "en\t<text>" per line
//                             stdout: one line each, the sentence vector as
//                             comma-separated floats, or empty when there is none
//   lumen-ocr embed-info      which embedding model each language uses here
//
// Runs on this Mac only (Vision framework, no network). PDFs are rendered page
// by page with PDFKit, up to MAX_PAGES. Exit 0 on success, 1 on a file Vision
// or PDFKit cannot open.
import Foundation
import Vision
import PDFKit
import ImageIO
import NaturalLanguage

let MAX_PAGES = 30
let MAX_CHARS = 120_000

func recognize(_ image: CGImage) -> String {
    let req = VNRecognizeTextRequest()
    req.recognitionLevel = .accurate
    req.usesLanguageCorrection = true
    var langs = ["zh-Hans", "zh-Hant", "en-US"]
    if #available(macOS 13.0, *) { langs += ["ja-JP", "ko-KR"] }
    if #available(macOS 12.0, *), let supported = try? req.supportedRecognitionLanguages() {
        langs = langs.filter { supported.contains($0) }
    }
    if !langs.isEmpty { req.recognitionLanguages = langs }
    req.minimumTextHeight = 0.012
    let handler = VNImageRequestHandler(cgImage: image, options: [:])
    guard (try? handler.perform([req])) != nil, let obs = req.results else { return "" }
    return obs.compactMap { $0.topCandidates(1).first?.string }.joined(separator: "\n")
}

func imageFile(_ url: URL) -> CGImage? {
    guard let src = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
    // Downscale huge photos: Vision does not need 48 MP to read a receipt.
    let opts: [CFString: Any] = [
        kCGImageSourceCreateThumbnailFromImageAlways: true,
        kCGImageSourceCreateThumbnailWithTransform: true,
        kCGImageSourceThumbnailMaxPixelSize: 3000,
    ]
    return CGImageSourceCreateThumbnailAtIndex(src, 0, opts as CFDictionary)
}

func pdfPages(_ url: URL, _ each: (CGImage) -> Bool) -> Bool {
    guard let doc = PDFDocument(url: url) else { return false }
    for i in 0..<min(doc.pageCount, MAX_PAGES) {
        guard let page = doc.page(at: i) else { continue }
        let box = page.bounds(for: .mediaBox)
        let scale = min(2.5, 2400 / max(box.width, box.height, 1))
        let size = CGSize(width: box.width * scale, height: box.height * scale)
        let img = page.thumbnail(of: size, for: .mediaBox)
        guard let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else { continue }
        if !each(cg) { break }
    }
    return true
}

// Text embeddings, on device, no downloads. For each language the best
// model this Mac already has wins:
//   1. NLEmbedding sentence embedding (macOS 11+; not every language/Mac)
//   2. NLContextualEmbedding, mean of token vectors (macOS 14+, only when
//      its assets are already on disk; we never request a download)
//   3. NLEmbedding word embedding, mean of word vectors
// A backend is chosen once per process, so every vector from one helper
// build on one Mac is comparable. `lumen-ocr embed-info` names them.
typealias Embed = (String) -> [Double]?

func mean(_ rows: [[Double]]) -> [Double]? {
    guard let first = rows.first, !first.isEmpty else { return nil }
    var acc = [Double](repeating: 0, count: first.count)
    for r in rows where r.count == acc.count { for i in 0..<r.count { acc[i] += r[i] } }
    return acc.map { $0 / Double(rows.count) }
}

func backend(_ lang: NLLanguage) -> (String, Embed)? {
    if let m = NLEmbedding.sentenceEmbedding(for: lang) {
        return ("sentence:\(m.dimension)", { m.vector(for: $0) })
    }
    if #available(macOS 14.0, *), let ce = NLContextualEmbedding(language: lang), ce.hasAvailableAssets,
       (try? ce.load()) != nil {
        return ("contextual:\(ce.dimension)", { text in
            guard let r = try? ce.embeddingResult(for: text, language: lang) else { return nil }
            var rows: [[Double]] = []
            r.enumerateTokenVectors(in: text.startIndex..<text.endIndex) { v, _ in rows.append(v); return true }
            return mean(rows)
        })
    }
    if let w = NLEmbedding.wordEmbedding(for: lang) {
        return ("word:\(w.dimension)", { text in
            let tok = NLTokenizer(unit: .word)
            tok.string = text
            var rows: [[Double]] = []
            tok.enumerateTokens(in: text.startIndex..<text.endIndex) { range, _ in
                if let v = w.vector(for: text[range].lowercased()) { rows.append(v) }
                return true
            }
            return mean(rows)
        })
    }
    return nil
}

func language(_ tag: String) -> NLLanguage { tag == "zh" ? .simplifiedChinese : .english }

func embedLoop() -> Never {
    var models: [String: Embed] = [:]
    var tried = Set<String>()
    func model(_ tag: String) -> Embed? {
        if !tried.contains(tag) {
            tried.insert(tag)
            models[tag] = backend(language(tag))?.1
        }
        return models[tag]
    }
    setvbuf(stdout, nil, _IOFBF, 1 << 16)
    while let line = readLine(strippingNewline: true) {
        var out = ""
        if let tab = line.firstIndex(of: "\t") {
            let tag = String(line[..<tab])
            let text = String(line[line.index(after: tab)...]).trimmingCharacters(in: .whitespaces)
            if !text.isEmpty, let m = model(tag), let v = m(text), v.contains(where: { $0 != 0 }) {
                out = v.map { String(format: "%.5f", $0) }.joined(separator: ",")
            }
        }
        print(out)
    }
    exit(0)
}

if CommandLine.arguments.count == 2 && CommandLine.arguments[1] == "embed-info" {
    for tag in ["zh", "en"] { print("\(tag)\t\(backend(language(tag))?.0 ?? "none")") }
    exit(0)
}
if CommandLine.arguments.count == 2 && CommandLine.arguments[1] == "embed" { embedLoop() }

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write("usage: lumen-ocr <file>\n".data(using: .utf8)!)
    exit(2)
}
let url = URL(fileURLWithPath: CommandLine.arguments[1])
var out = ""
if url.pathExtension.lowercased() == "pdf" {
    let ok = pdfPages(url) { img in
        out += recognize(img) + "\n"
        return out.count < MAX_CHARS
    }
    if !ok { exit(1) }
} else {
    guard let img = imageFile(url) else { exit(1) }
    out = recognize(img)
}
print(out.trimmingCharacters(in: .whitespacesAndNewlines))

// lumen-ocr: print the text Apple Vision reads in an image or a scanned PDF.
//
//   lumen-ocr <file>          text on stdout, nothing when there is none
//
// Runs on this Mac only (Vision framework, no network). PDFs are rendered page
// by page with PDFKit, up to MAX_PAGES. Exit 0 on success, 1 on a file Vision
// or PDFKit cannot open.
import Foundation
import Vision
import PDFKit
import ImageIO

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

// Renders the editor page that clarity.sh shares: real code in a monospaced font, a light pane
// and a dark pane, at a Retina size. Writes base.png (the typed line still empty), full.png (the
// line typed) and layout.json (where the line and its characters are) into the given directory.
//
//   swift render.swift <out dir> <code file> <width> <height>

import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

let args = CommandLine.arguments
guard args.count == 5, let width = Int(args[3]), let height = Int(args[4]) else {
    FileHandle.standardError.write("usage: render.swift <out dir> <code file> <width> <height>\n".data(using: .utf8)!)
    exit(1)
}
let outDir = URL(fileURLWithPath: args[1])
let code = try String(contentsOfFile: args[2], encoding: .utf8)
    .components(separatedBy: "\n")
    .map { $0.replacingOccurrences(of: "\t", with: "    ") }

// 13 pt Menlo at 2x, like an editor on a Retina display.
let fontSize: CGFloat = 26
let lineHeight: CGFloat = 40
let statusBar: CGFloat = 56
let gutter: CGFloat = 96
let font = CTFontCreateWithName("Menlo" as CFString, fontSize, nil)
var glyph = CGGlyph(0)
var space: UniChar = 0x20
CTFontGetGlyphsForCharacters(font, &space, &glyph, 1)
var advanceSize = CGSize.zero
CTFontGetAdvancesForGlyphs(font, .horizontal, &glyph, &advanceSize, 1)
let advance = advanceSize.width

struct Theme {
    let background: CGColor
    let text: CGColor
    let keyword: CGColor
    let string: CGColor
    let comment: CGColor
    let lineNumber: CGColor
}
func rgb(_ hex: UInt32) -> CGColor {
    CGColor(
        red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
        blue: CGFloat(hex & 0xff) / 255, alpha: 1)
}
let light = Theme(
    background: rgb(0xffffff), text: rgb(0x1f1f1f), keyword: rgb(0xaf00db),
    string: rgb(0xa31515), comment: rgb(0x008000), lineNumber: rgb(0x8a8a8a))
let dark = Theme(
    background: rgb(0x1e1e1e), text: rgb(0xd4d4d4), keyword: rgb(0x569cd6),
    string: rgb(0xce9178), comment: rgb(0x6a9955), lineNumber: rgb(0x858585))

let keywords: Set<String> = [
    "fn", "let", "mut", "pub", "use", "struct", "enum", "impl", "match", "if", "else", "for",
    "while", "loop", "return", "self", "Self", "async", "await", "const", "mod", "crate", "move",
    "in", "as", "where", "true", "false", "Some", "None", "Ok", "Err",
]

/// Splits a line into coloured runs: comments, strings, keywords and the rest.
func runs(_ line: String, _ theme: Theme) -> [(String, CGColor)] {
    if let comment = line.range(of: "//") {
        return runs(String(line[..<comment.lowerBound]), theme)
            + [(String(line[comment.lowerBound...]), theme.comment)]
    }
    var result: [(String, CGColor)] = []
    var word = ""
    var inString = false
    func flush() {
        if !word.isEmpty {
            result.append((word, keywords.contains(word) ? theme.keyword : theme.text))
            word = ""
        }
    }
    for ch in line {
        if inString {
            result[result.count - 1].0.append(ch)
            if ch == "\"" { inString = false }
        } else if ch == "\"" {
            flush()
            result.append(("\"", theme.string))
            inString = true
        } else if ch.isLetter || ch.isNumber || ch == "_" {
            word.append(ch)
        } else {
            flush()
            result.append((String(ch), theme.text))
        }
    }
    flush()
    return result
}

func draw(_ ctx: CGContext, _ text: String, x: CGFloat, top: CGFloat, color: CGColor) {
    let attributes: [NSAttributedString.Key: Any] = [
        NSAttributedString.Key(kCTFontAttributeName as String): font,
        NSAttributedString.Key(kCTForegroundColorAttributeName as String): color,
    ]
    let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attributes))
    // CoreGraphics counts y from the bottom; place the baseline inside the line box.
    ctx.textPosition = CGPoint(x: x, y: CGFloat(height) - top - lineHeight + 11)
    CTLineDraw(line, ctx)
}

/// The line typed during the run, in the light pane on row `typedRow`.
let typedText = "let encoder = check_screen_share_encoder(&inner, codec).await?;"
let typedRow = 18
let indent = 8

func render(typed: Bool) -> CGImage {
    let ctx = CGContext(
        data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
    ctx.setShouldSmoothFonts(false)
    let paneWidth = CGFloat(width / 2)
    let rows = Int((CGFloat(height) - statusBar) / lineHeight)
    for (pane, theme) in [light, dark].enumerated() {
        let left = CGFloat(pane) * paneWidth
        ctx.setFillColor(theme.background)
        ctx.fill(CGRect(x: left, y: 0, width: paneWidth, height: CGFloat(height) - statusBar))
        let maxColumns = Int((paneWidth - gutter - 24) / advance)
        for row in 0..<rows {
            let top = statusBar + CGFloat(row) * lineHeight
            let source = pane == 0 ? row : row + 60
            draw(ctx, String(format: "%4d", source + 1), x: left + 8, top: top, color: theme.lineNumber)
            var text = source < code.count ? code[source] : ""
            if pane == 0 && row == typedRow {
                text = typed ? String(repeating: " ", count: indent) + typedText : ""
            }
            text = String(text.prefix(maxColumns))
            var x = left + gutter
            for (run, color) in runs(text, theme) {
                draw(ctx, run, x: x, top: top, color: color)
                x += CGFloat(run.count) * advance
            }
        }
    }
    // The status bar stays plain: the harness writes its frame id there.
    ctx.setFillColor(rgb(0x3c3c3c))
    ctx.fill(CGRect(x: 0, y: CGFloat(height) - statusBar, width: CGFloat(width), height: statusBar))
    return ctx.makeImage()!
}

func write(_ image: CGImage, _ name: String) throws {
    let url = outDir.appendingPathComponent(name)
    guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else {
        throw NSError(domain: "render", code: 1)
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else { throw NSError(domain: "render", code: 2) }
}

try FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)
try write(render(typed: false), "base.png")
try write(render(typed: true), "full.png")
let lineX = gutter + CGFloat(indent) * advance
let layout: [String: Any] = [
    "width": width,
    "height": height,
    "line_top": Int(statusBar + CGFloat(typedRow) * lineHeight),
    "line_height": Int(lineHeight),
    // Where the caret sits after 0, 1, ... characters.
    "char_x": (0...typedText.count).map { Int((lineX + CGFloat($0) * advance).rounded()) },
    "status_bar": Int(statusBar),
]
try JSONSerialization.data(withJSONObject: layout, options: [.prettyPrinted, .sortedKeys])
    .write(to: outDir.appendingPathComponent("layout.json"))

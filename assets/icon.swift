// Regenerate: swift assets/icon.swift assets/icon.png
import AppKit

let S: CGFloat = 1024
let cs = CGColorSpaceCreateDeviceRGB()
let ctx = CGContext(data: nil, width: Int(S), height: Int(S), bitsPerComponent: 8, bytesPerRow: 0,
                    space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
ctx.translateBy(x: 0, y: S)
ctx.scaleBy(x: 1, y: -1) // top-left origin

func rgb(_ h: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(red: CGFloat((h >> 16) & 255) / 255, green: CGFloat((h >> 8) & 255) / 255, blue: CGFloat(h & 255) / 255, alpha: a)
}
func rr(_ r: CGRect, _ rad: CGFloat) -> CGPath { CGPath(roundedRect: r, cornerWidth: rad, cornerHeight: rad, transform: nil) }
func fill(_ p: CGPath, _ c: CGColor) { ctx.addPath(p); ctx.setFillColor(c); ctx.fillPath() }

// Squircle background (Apple grid: 824pt body inside 1024 canvas)
let bg = CGRect(x: 100, y: 100, width: 824, height: 824)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -14), blur: 30, color: rgb(0x000000, 0.35))
fill(rr(bg, 185), rgb(0x2B3A9E))
ctx.restoreGState()
ctx.saveGState()
ctx.addPath(rr(bg, 185)); ctx.clip()
let grad = CGGradient(colorsSpace: cs, colors: [rgb(0x5C7CFA), rgb(0x2B3A9E), rgb(0x1B2470)] as CFArray, locations: [0, 0.6, 1])!
ctx.drawLinearGradient(grad, start: CGPoint(x: 300, y: 100), end: CGPoint(x: 724, y: 924), options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
ctx.restoreGState()

// Table card
let card = CGRect(x: 212, y: 236, width: 600, height: 552)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 24, color: rgb(0x0B1040, 0.45))
fill(rr(card, 46), rgb(0xFFFFFF))
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(rr(card, 46)); ctx.clip()
let headH: CGFloat = 168
let colW = card.width / 3
ctx.setFillColor(rgb(0xE8EDFF)); ctx.fill(CGRect(x: card.minX, y: card.minY, width: card.width, height: headH))

// Column profiles in the header: mini histograms / top-value bars
let accent = rgb(0x4C6EF5)
let hists: [[CGFloat]] = [[0.35, 0.7, 1.0, 0.6, 0.3], [0.9, 0.55, 0.3, 0.2, 0.12], [0.2, 0.45, 0.8, 1.0, 0.55]]
for (c, h) in hists.enumerated() {
    let x0 = card.minX + CGFloat(c) * colW + 34
    let base = card.minY + headH - 30
    let bw: CGFloat = 20, gap: CGFloat = 8
    for (i, v) in h.enumerated() {
        let bh = v * 92
        fill(rr(CGRect(x: x0 + CGFloat(i) * (bw + gap), y: base - bh, width: bw, height: bh), 5), accent)
    }
}

// Grid lines
ctx.setStrokeColor(rgb(0xD3DAF0)); ctx.setLineWidth(6)
for c in 1..<3 {
    let x = card.minX + CGFloat(c) * colW
    ctx.move(to: CGPoint(x: x, y: card.minY)); ctx.addLine(to: CGPoint(x: x, y: card.maxY))
}
let rowH = (card.height - headH) / 4
for r in 0..<4 {
    let y = card.minY + headH + CGFloat(r) * rowH
    ctx.move(to: CGPoint(x: card.minX, y: y)); ctx.addLine(to: CGPoint(x: card.maxX, y: y))
}
ctx.strokePath()

// Cell placeholders: text left-aligned, numbers right-aligned in the last column
let widths: [[CGFloat]] = [[120, 90, 70], [80, 130, 100], [140, 70, 56], [100, 110, 88]]
for r in 0..<4 {
    let cy = card.minY + headH + CGFloat(r) * rowH + rowH / 2
    for c in 0..<3 {
        let w = widths[r][c]
        let x = c == 2 ? card.minX + CGFloat(c + 1) * colW - 30 - w : card.minX + CGFloat(c) * colW + 30
        fill(rr(CGRect(x: x, y: cy - 11, width: w, height: 22), 11), rgb(c == 2 ? 0x9AA6CC : 0xC3CBE3))
    }
}

// Selected cell
let sel = CGRect(x: card.minX + colW + 3, y: card.minY + headH + rowH + 3, width: colW - 6, height: rowH - 6)
ctx.addPath(rr(sel, 8)); ctx.setStrokeColor(accent); ctx.setLineWidth(10); ctx.strokePath()
ctx.restoreGState()

let img = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: img)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))

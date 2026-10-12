// Draws the menu bar version of the freemonplay mark as a vector PDF template image
// (black on transparent; macOS tints it for light/dark menu bars and highlighting).
// Usage: swift make-menubar-icon.swift menubar.pdf [preview.png]
//
// Redrawn for 18 pt rather than scaled down from the app icon: thicker strokes, bigger
// screens, and each screen is a solid square with a hollow "glass" so it still reads as a
// screen at that size.

import AppKit

let side: CGFloat = 18
let args = CommandLine.arguments
let pdfPath = args.count > 1 ? args[1] : "menubar.pdf"
let previewPath = args.count > 2 ? args[2] : nil

func roundedRect(_ rect: CGRect, _ radius: CGFloat) -> CGPath {
    CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
}

/// The mark in an 18 x 18 pt box, origin bottom-left.
func drawMark(_ ctx: CGContext) {
    ctx.setFillColor(CGColor(gray: 0, alpha: 1))
    let stemX: CGFloat = 7.6
    let stemWidth: CGFloat = 2.6
    let stemBottom: CGFloat = 3.0
    let stemTop: CGFloat = 12.4
    let hookCentre = CGPoint(x: stemX + 3.0, y: stemTop)
    let hookRadius: CGFloat = 3.0
    let hookEnd: CGFloat = .pi * 0.40
    let crossY: CGFloat = 10.0
    let screen: CGFloat = 4.6
    let tip = CGPoint(x: hookCentre.x + hookRadius * cos(hookEnd), y: hookCentre.y + hookRadius * sin(hookEnd))
    let screens = [
        CGPoint(x: tip.x + 1.6, y: tip.y + 0.3),  // end of the hook
        CGPoint(x: 3.3, y: crossY),                // crossbar, left
        CGPoint(x: 12.3, y: crossY),               // crossbar, right
        CGPoint(x: stemX, y: stemBottom - 0.3),    // foot
    ]
    func screenRect(_ c: CGPoint) -> CGRect {
        CGRect(x: c.x - screen / 2, y: c.y - screen / 2, width: screen, height: screen)
    }

    // The strokes stop at each screen's edge, so they never show through its glass.
    ctx.saveGState()
    let clip = CGMutablePath()
    clip.addRect(CGRect(x: 0, y: 0, width: side, height: side))
    for c in screens { clip.addPath(roundedRect(screenRect(c), 1.0)) }
    ctx.addPath(clip)
    ctx.clip(using: .evenOdd)

    // Stem.
    ctx.fill(CGRect(x: stemX - stemWidth / 2, y: stemBottom, width: stemWidth, height: stemTop - stemBottom + 0.05))

    // Hook, tapering like a serif stroke.
    let hook = CGMutablePath()
    let steps = 48
    func hookPoint(_ t: CGFloat, outer: Bool) -> CGPoint {
        let angle = .pi - (.pi - hookEnd) * t
        let half = (stemWidth - 0.8 * t) / 2
        let r = hookRadius + (outer ? half : -half)
        return CGPoint(x: hookCentre.x + r * cos(angle), y: hookCentre.y + r * sin(angle))
    }
    hook.move(to: hookPoint(0, outer: true))
    for i in 1...steps { hook.addLine(to: hookPoint(CGFloat(i) / CGFloat(steps), outer: true)) }
    for i in stride(from: steps, through: 0, by: -1) { hook.addLine(to: hookPoint(CGFloat(i) / CGFloat(steps), outer: false)) }
    hook.closeSubpath()
    ctx.addPath(hook)
    ctx.fillPath()

    // Crossbar.
    ctx.fill(CGRect(x: 3.6, y: crossY - 0.85, width: 8.4, height: 1.7))

    ctx.restoreGState()

    // Screens: solid square with a hollow centre (even-odd fill).
    for c in screens {
        let outer = screenRect(c)
        let path = CGMutablePath()
        path.addPath(roundedRect(outer, 1.0))
        path.addPath(roundedRect(outer.insetBy(dx: 1.15, dy: 1.15), 0.45))
        ctx.addPath(path)
        ctx.fillPath(using: .evenOdd)
    }
}

// Vector PDF for the app.
var box = CGRect(x: 0, y: 0, width: side, height: side)
let pdf = CGContext(URL(fileURLWithPath: pdfPath) as CFURL, mediaBox: &box, nil)!
pdf.beginPDFPage(nil)
drawMark(pdf)
pdf.endPDFPage()
pdf.closePDF()
print("wrote \(pdfPath)")

// Optional preview: the mark at 1x, 2x and 8x, black and white-on-dark, for checking.
if let previewPath {
    let scales: [CGFloat] = [1, 2, 8]
    var row: CGFloat = 0
    for s in scales { row += side * s + 10 }
    let width = Int(row * 2 + 10)
    let height = Int(side * 8 + 20)
    let ctx = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
                        space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.setFillColor(CGColor(gray: 0.93, alpha: 1))
    ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
    var x: CGFloat = 10
    for dark in [false, true] {
        for s in scales {
            let rect = CGRect(x: x, y: 10, width: side * s, height: side * s)
            if dark {
                ctx.setFillColor(CGColor(gray: 0.15, alpha: 1))
                ctx.fill(rect)
            }
            ctx.saveGState()
            ctx.translateBy(x: rect.minX, y: rect.minY)
            ctx.scaleBy(x: s, y: s)
            ctx.beginTransparencyLayer(auxiliaryInfo: nil)
            drawMark(ctx)
            if dark {
                // Tint like macOS does for a template image on a dark menu bar.
                ctx.setBlendMode(.sourceIn)
                ctx.setFillColor(CGColor(gray: 1, alpha: 1))
                ctx.fill(CGRect(x: 0, y: 0, width: side, height: side))
            }
            ctx.endTransparencyLayer()
            ctx.restoreGState()
            x += side * s + 10
        }
    }
    let rep = NSBitmapImageRep(cgImage: ctx.makeImage()!)
    try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: previewPath))
    print("wrote \(previewPath)")
}

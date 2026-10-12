// Draws the Freemonplay app icon (1024x1024 PNG) with CoreGraphics.
// Usage: swift make-icon.swift out.png [graphite|sunset|emerald]   (graphite is the app icon)
//
// Plain shapes only: Apple's SF Symbols license doesn't allow symbols in app icons.
// Layout follows the macOS icon grid: an 824 pt rounded square centred on a 1024 canvas.

import AppKit

let size: CGFloat = 1024
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon.png"
let paletteName = CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "graphite"

func rgb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> CGColor {
    CGColor(srgbRed: r / 255, green: g / 255, blue: b / 255, alpha: a)
}

func roundedRect(_ rect: CGRect, _ radius: CGFloat) -> CGPath {
    CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
}

func linearGradient(_ ctx: CGContext, _ colors: [CGColor], from: CGPoint, to: CGPoint) {
    let gradient = CGGradient(colorsSpace: CGColorSpace(name: CGColorSpace.sRGB), colors: colors as CFArray, locations: nil)!
    ctx.drawLinearGradient(gradient, start: from, end: to, options: [])
}

/// Background gradient (top, bottom), fill under the shadow, and screen panel gradient.
struct Palette {
    let top: CGColor, bottom: CGColor, base: CGColor, panelA: CGColor, panelB: CGColor
}
let palettes: [String: Palette] = [
    "sunset": Palette(top: rgb(255, 166, 72), bottom: rgb(222, 48, 118), base: rgb(235, 90, 100),
                      panelA: rgb(96, 38, 140), panelB: rgb(52, 18, 96)),
    "emerald": Palette(top: rgb(70, 214, 150), bottom: rgb(8, 120, 88), base: rgb(30, 160, 110),
                       panelA: rgb(40, 52, 72), panelB: rgb(18, 26, 42)),
    "graphite": Palette(top: rgb(88, 96, 110), bottom: rgb(24, 28, 38), base: rgb(50, 56, 68),
                        panelA: rgb(255, 200, 70), panelB: rgb(245, 120, 30)),
]
guard let palette = palettes[paletteName] else { fatalError("unknown palette \(paletteName)") }

let space = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
                    space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
// CoreGraphics' origin is bottom-left; y grows upwards.

// Body: rounded square with a soft drop shadow and a blue-to-indigo gradient.
let body = CGRect(x: 100, y: 100, width: 824, height: 824)
let bodyPath = roundedRect(body, 186)
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: rgb(0, 0, 0, 0.35))
ctx.addPath(bodyPath)
ctx.setFillColor(palette.base)
ctx.fillPath()
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(bodyPath)
ctx.clip()
linearGradient(ctx, [palette.top, palette.bottom], from: CGPoint(x: 0, y: body.maxY), to: CGPoint(x: 0, y: body.minY))
// Gentle sheen across the top.
linearGradient(ctx, [rgb(255, 255, 255, 0.16), rgb(255, 255, 255, 0)],
               from: CGPoint(x: 0, y: body.maxY), to: CGPoint(x: 0, y: body.midY))
ctx.restoreGState()

// A serif lowercase "f" whose four tips end in small square screens (the screens are its
// serifs): the end of the top hook, both ends of the crossbar, and the foot. Drawn from
// shapes, not a font.
let white = rgb(255, 255, 255)
let stemX: CGFloat = 468           // centre line of the stem
let stemBottom: CGFloat = 268
let stemTop: CGFloat = 600
let stemWidth: CGFloat = 104
let hookCentre = CGPoint(x: stemX + 132, y: stemTop)
let hookRadius: CGFloat = 132
let hookEnd: CGFloat = .pi * 0.40  // where the hook's stroke stops (angle on its circle)
let crossY: CGFloat = 528
let crossLeft: CGFloat = 318
let crossRight: CGFloat = 630
let screenSize: CGFloat = 138

// One shared shadow for the whole mark.
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 24, color: rgb(0, 0, 0, 0.45))
ctx.beginTransparencyLayer(auxiliaryInfo: nil)
ctx.setFillColor(white)

// Stem.
ctx.fill(CGRect(x: stemX - stemWidth / 2, y: stemBottom, width: stemWidth, height: stemTop - stemBottom + 1))

// Hook: one filled shape, full stem width where it leaves the stem, tapering towards the
// tip (the thick-thin contrast of a serif face).
let hook = CGMutablePath()
let steps = 64
func hookPoint(_ t: CGFloat, outer: Bool) -> CGPoint {
    let angle = .pi - (.pi - hookEnd) * t
    let halfWidth = (stemWidth - 44 * t) / 2
    let r = hookRadius + (outer ? halfWidth : -halfWidth)
    return CGPoint(x: hookCentre.x + r * cos(angle), y: hookCentre.y + r * sin(angle))
}
hook.move(to: hookPoint(0, outer: true))
for i in 1...steps { hook.addLine(to: hookPoint(CGFloat(i) / CGFloat(steps), outer: true)) }
for i in stride(from: steps, through: 0, by: -1) { hook.addLine(to: hookPoint(CGFloat(i) / CGFloat(steps), outer: false)) }
hook.closeSubpath()
ctx.addPath(hook)
ctx.fillPath()

// Crossbar: thinner than the stem.
ctx.fill(CGRect(x: crossLeft, y: crossY - 31, width: crossRight - crossLeft, height: 62))

// A square screen: white bezel and a glowing panel.
func screen(at centre: CGPoint) {
    let bezel = CGRect(x: centre.x - screenSize / 2, y: centre.y - screenSize / 2, width: screenSize, height: screenSize)
    ctx.addPath(roundedRect(bezel, screenSize * 0.16))
    ctx.setFillColor(white)
    ctx.fillPath()
    let panel = bezel.insetBy(dx: screenSize * 0.13, dy: screenSize * 0.13)
    ctx.saveGState()
    ctx.addPath(roundedRect(panel, screenSize * 0.07))
    ctx.clip()
    linearGradient(ctx, [palette.panelA, palette.panelB], from: CGPoint(x: panel.minX, y: panel.maxY),
                   to: CGPoint(x: panel.maxX, y: panel.minY))
    ctx.restoreGState()
}

let tipAngle = hookEnd
let tip = CGPoint(x: hookCentre.x + hookRadius * cos(tipAngle), y: hookCentre.y + hookRadius * sin(tipAngle))
screen(at: CGPoint(x: tip.x + 44, y: tip.y + 4))                 // end of the hook
screen(at: CGPoint(x: crossLeft, y: crossY))                     // crossbar, left
screen(at: CGPoint(x: crossRight, y: crossY))                    // crossbar, right
screen(at: CGPoint(x: stemX, y: stemBottom))                     // foot

ctx.endTransparencyLayer()
ctx.restoreGState()

let image = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: image)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
print("wrote \(out)")

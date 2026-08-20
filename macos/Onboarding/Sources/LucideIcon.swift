import SwiftUI

/// A few [Lucide](https://lucide.dev) icons (ISC licence) drawn as vector paths.
///
/// Drawn rather than shipped as files on purpose: no asset catalog and no
/// `Bundle.module`, whose absence crashed the shipped wizard once (v1.2.6), so
/// there is nothing to copy into the .app and nothing to go missing at runtime.
/// They inherit the current foreground colour and stay crisp at any size, the
/// way SF Symbols do.
///
/// Geometry is transcribed from the official 24×24 sources, keeping Lucide's
/// 2 pt round-capped stroke — scaled with the icon so a 14 pt icon looks like
/// Lucide at 14 pt, not like a shrunk 24 pt one.
struct LucideIcon: View {
    enum Glyph {
        /// `clipboard-paste` — paste what's on the clipboard into a field.
        case clipboardPaste
        /// `copy` — copy this value to the clipboard.
        case copy
    }

    let glyph: Glyph
    var size: CGFloat = 16

    var body: some View {
        LucideShape(glyph: glyph)
            .stroke(style: StrokeStyle(lineWidth: 2 * size / 24,
                                       lineCap: .round,
                                       lineJoin: .round))
            .frame(width: size, height: size)
    }
}

private struct LucideShape: Shape {
    let glyph: LucideIcon.Glyph

    func path(in rect: CGRect) -> Path {
        var p = Path()
        switch glyph {
        case .clipboardPaste: clipboardPaste(&p)
        case .copy: copy(&p)
        }
        let s = min(rect.width, rect.height) / 24
        return p.applying(CGAffineTransform(scaleX: s, y: s))
    }

    /// <path d="M11 14h10"/><path d="M16 4h2a2 2 0 0 1 2 2v1.344"/>
    /// <path d="m17 18 4-4-4-4"/>
    /// <path d="M8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 1.793-1.113"/>
    /// <rect x="8" y="2" width="8" height="4" rx="1"/>
    private func clipboardPaste(_ p: inout Path) {
        p.move(to: CGPoint(x: 11, y: 14))
        p.addLine(to: CGPoint(x: 21, y: 14))

        p.move(to: CGPoint(x: 16, y: 4))
        p.addLine(to: CGPoint(x: 18, y: 4))
        p.addArc(tangent1End: CGPoint(x: 20, y: 4), tangent2End: CGPoint(x: 20, y: 6), radius: 2)
        p.addLine(to: CGPoint(x: 20, y: 7.344))

        p.move(to: CGPoint(x: 17, y: 18))
        p.addLine(to: CGPoint(x: 21, y: 14))
        p.addLine(to: CGPoint(x: 17, y: 10))

        p.move(to: CGPoint(x: 8, y: 4))
        p.addLine(to: CGPoint(x: 6, y: 4))
        p.addArc(tangent1End: CGPoint(x: 4, y: 4), tangent2End: CGPoint(x: 4, y: 6), radius: 2)
        p.addLine(to: CGPoint(x: 4, y: 20))
        p.addArc(tangent1End: CGPoint(x: 4, y: 22), tangent2End: CGPoint(x: 6, y: 22), radius: 2)
        p.addLine(to: CGPoint(x: 18, y: 22))
        // The source ends on a partial arc (…a2 2 0 0 0 1.793-1.113); a quadratic
        // through the same corner is indistinguishable at icon sizes.
        p.addQuadCurve(to: CGPoint(x: 19.793, y: 20.887), control: CGPoint(x: 19.2, y: 22))

        p.addPath(Path(roundedRect: CGRect(x: 8, y: 2, width: 8, height: 4), cornerRadius: 1))
    }

    /// <rect width="14" height="14" x="8" y="8" rx="2"/>
    /// <path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/>
    private func copy(_ p: inout Path) {
        p.addPath(Path(roundedRect: CGRect(x: 8, y: 8, width: 14, height: 14), cornerRadius: 2))

        p.move(to: CGPoint(x: 4, y: 16))
        p.addCurve(to: CGPoint(x: 2, y: 14),
                   control1: CGPoint(x: 2.9, y: 16), control2: CGPoint(x: 2, y: 15.1))
        p.addLine(to: CGPoint(x: 2, y: 4))
        p.addCurve(to: CGPoint(x: 4, y: 2),
                   control1: CGPoint(x: 2, y: 2.9), control2: CGPoint(x: 2.9, y: 2))
        p.addLine(to: CGPoint(x: 14, y: 2))
        p.addCurve(to: CGPoint(x: 16, y: 4),
                   control1: CGPoint(x: 15.1, y: 2), control2: CGPoint(x: 16, y: 2.9))
    }
}

/// A bare icon button: no background, no border, just the glyph — it dims on
/// hover and press so it still reads as clickable.
struct IconButton: View {
    let glyph: LucideIcon.Glyph
    var size: CGFloat = 16
    var help: String = ""
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            LucideIcon(glyph: glyph, size: size)
                .foregroundStyle(Color.brandGreen.opacity(hovering ? 1 : 0.55))
                .padding(4) // click target, not a visible box
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(help)
    }
}

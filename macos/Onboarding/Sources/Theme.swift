import SwiftUI

// Brand colors — PADDOCK palette from brandkit/README.md. "Jamais d'autre
// couleur que celles listées" — these four are the entire allowed set.
extension Color {
    static let brandGreen  = Color(red: 0x0D/255, green: 0x2E/255, blue: 0x25/255) // #0D2E25 Racing Green
    static let brandCitron = Color(red: 0xCE/255, green: 0xDC/255, blue: 0x00/255) // #CEDC00 Signal Citron
    static let brandCream  = Color(red: 0xEF/255, green: 0xEA/255, blue: 0xD8/255) // #EFEAD8 Chamois Cream
    static let brandOnyx   = Color(red: 0x1A/255, green: 0x1A/255, blue: 0x1A/255) // #1A1A1A Onyx
}

// MARK: - Button styles

/// Citron-filled primary CTA. Soft-rounded (12pt — squircle-flavored
/// without going full pill), generous vertical padding, racing-green text
/// on citron for the brand contrast. Disabled state dims to 0.7.
struct BrandPrimaryButtonStyle: ButtonStyle {
    var enabled: Bool = true

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 15, weight: .semibold))
            .foregroundStyle(enabled ? Color.brandGreen : Color.brandGreen.opacity(0.5))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(enabled ? Color.brandCitron : Color.brandCitron.opacity(0.35))
            )
            .scaleEffect(configuration.isPressed && enabled ? 0.97 : 1.0)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
            .opacity(enabled ? 1.0 : 0.7)
    }
}

/// Compact secondary action — used by per-row Grant buttons in the
/// permissions step. Citron pill, racing-green label, 8pt corners.
struct BrandRowButtonStyle: ButtonStyle {
    var prominent: Bool = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(prominent ? Color.brandGreen : Color.brandGreen.opacity(0.85))
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(prominent ? Color.brandCitron : Color.brandCream)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(Color.brandGreen.opacity(prominent ? 0 : 0.15), lineWidth: 1)
            )
            .scaleEffect(configuration.isPressed ? 0.96 : 1.0)
            .animation(.easeOut(duration: 0.1), value: configuration.isPressed)
    }
}

/// Quiet "Granted ✓" pill — no action, just a status badge.
struct BrandRowBadge: View {
    let text: String
    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: "checkmark")
                .font(.system(size: 10, weight: .bold))
            Text(text)
                .font(.system(size: 12, weight: .semibold))
        }
        .foregroundStyle(Color.brandGreen)
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(Color.brandCitron.opacity(0.55))
        )
    }
}

/// The real whisper-push logo: 3 S-curves (sound waves).
/// Reproduces the paths from icon.svg exactly, scaled to fit.
struct WPLogo: View {
    var animate: Bool = false
    var progress: CGFloat = 0  // 0..1 for download fill

    @State private var phase: CGFloat = 0

    var body: some View {
        ZStack {
            // 3 waves, offset vertically
            WavePath(yOffset: -0.3)
                .trim(from: 0, to: progress > 0 ? progress : 1)
                .stroke(
                    Color.brandCitron.opacity(progress > 0 && progress < 1 ? 1 : 1),
                    style: StrokeStyle(lineWidth: 3.5, lineCap: .round, lineJoin: .round)
                )

            if progress > 0 && progress < 1 {
                WavePath(yOffset: -0.3)
                    .trim(from: progress, to: 1)
                    .stroke(
                        Color.brandCitron.opacity(0.2),
                        style: StrokeStyle(lineWidth: 3.5, lineCap: .round, lineJoin: .round)
                    )
            }

            WavePath(yOffset: 0)
                .trim(from: 0, to: progress > 0 ? progress : 1)
                .stroke(
                    Color.brandCitron,
                    style: StrokeStyle(lineWidth: 3.5, lineCap: .round, lineJoin: .round)
                )

            if progress > 0 && progress < 1 {
                WavePath(yOffset: 0)
                    .trim(from: progress, to: 1)
                    .stroke(
                        Color.brandCitron.opacity(0.2),
                        style: StrokeStyle(lineWidth: 3.5, lineCap: .round, lineJoin: .round)
                    )
            }

            WavePath(yOffset: 0.3)
                .trim(from: 0, to: progress > 0 ? progress : 1)
                .stroke(
                    Color.brandCitron,
                    style: StrokeStyle(lineWidth: 3.5, lineCap: .round, lineJoin: .round)
                )

            if progress > 0 && progress < 1 {
                WavePath(yOffset: 0.3)
                    .trim(from: progress, to: 1)
                    .stroke(
                        Color.brandCitron.opacity(0.2),
                        style: StrokeStyle(lineWidth: 3.5, lineCap: .round, lineJoin: .round)
                    )
            }
        }
        .scaleEffect(animate ? 1.05 : 0.95)
        .animation(
            animate ? .easeInOut(duration: 1.5).repeatForever(autoreverses: true) : .default,
            value: animate
        )
    }
}

/// One S-curve wave, matching the icon.svg shape.
/// yOffset: -0.3, 0, 0.3 for the three waves.
struct WavePath: Shape {
    let yOffset: CGFloat

    func path(in rect: CGRect) -> Path {
        var path = Path()
        let w = rect.width
        let h = rect.height
        let midY = rect.midY + yOffset * h

        // S-curve: descends from left, curves down to middle, curves up to right
        // Matches the icon.svg sinusoidal shape
        let amplitude = h * 0.15

        path.move(to: CGPoint(x: 0, y: midY + amplitude))

        // First half: curve down
        path.addCurve(
            to: CGPoint(x: w * 0.5, y: midY),
            control1: CGPoint(x: w * 0.25, y: midY - amplitude * 0.5),
            control2: CGPoint(x: w * 0.35, y: midY - amplitude)
        )

        // Second half: curve up
        path.addCurve(
            to: CGPoint(x: w, y: midY - amplitude),
            control1: CGPoint(x: w * 0.65, y: midY + amplitude),
            control2: CGPoint(x: w * 0.75, y: midY + amplitude * 0.5)
        )

        return path
    }
}

/// Brand squircle container with the logo inside.
struct LogoSquircle: View {
    var animate: Bool = false
    var progress: CGFloat = 0
    var size: CGFloat = 96

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.25)
                .fill(Color.brandGreen)
                .frame(width: size, height: size)
                .shadow(color: .black.opacity(0.2), radius: 12, y: 6)

            WPLogo(animate: animate, progress: progress)
                .frame(width: size * 0.58, height: size * 0.42)
        }
    }
}

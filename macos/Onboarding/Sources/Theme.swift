import SwiftUI

// PADDOCK brand palette from brandkit/README.md — these four are the
// entire allowed set ("jamais d'autre couleur que celles listées").
extension Color {
    static let brandGreen  = Color(red: 0x0D/255, green: 0x2E/255, blue: 0x25/255) // #0D2E25 Racing Green
    static let brandCitron = Color(red: 0xCE/255, green: 0xDC/255, blue: 0x00/255) // #CEDC00 Signal Citron
    static let brandCream  = Color(red: 0xEF/255, green: 0xEA/255, blue: 0xD8/255) // #EFEAD8 Chamois Cream
    static let brandOnyx   = Color(red: 0x1A/255, green: 0x1A/255, blue: 0x1A/255) // #1A1A1A Onyx
}

// MARK: - Button styles

/// Citron-filled primary CTA. Soft-rounded squircle, racing-green text.
/// Disabled state dims to 0.7 opacity.
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

/// Compact per-row Grant button (Permissions step).
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

/// Rounded-square checkbox inspired by the checkbox-13 CSS pattern — 8pt
/// corner radius (a touch more rounded than the 7px reference). Citron
/// fill when checked with a racing-green checkmark inside; transparent
/// + soft racing-green border when unchecked.
struct BrandCheckbox: View {
    let isOn: Bool
    var size: CGFloat = 20

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(isOn ? Color.brandCitron : Color.clear)
                .frame(width: size, height: size)
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(isOn ? Color.clear : Color.brandGreen.opacity(0.35), lineWidth: 1.5)
                .frame(width: size, height: size)
            if isOn {
                Image(systemName: "checkmark")
                    .font(.system(size: size * 0.55, weight: .bold))
                    .foregroundStyle(Color.brandGreen)
            }
        }
    }
}

// MARK: - Logo

/// Brand kit AppIcon rendered as a soft-shadowed image. Replaces the
/// previous hand-coded squircle + wave reconstruction with the official
/// PNG asset bundled in the Swift Package resources.
struct LogoSquircle: View {
    var animate: Bool = false
    var size: CGFloat = 96

    @State private var breathing = false

    var body: some View {
        Image("AppIcon", bundle: .module)
            .resizable()
            .interpolation(.high)
            .scaledToFit()
            .frame(width: size, height: size)
            .shadow(color: Color.brandGreen.opacity(0.25), radius: 12, y: 6)
            .scaleEffect(animate && breathing ? 1.04 : 1.0)
            .animation(
                animate ? .easeInOut(duration: 1.6).repeatForever(autoreverses: true) : .default,
                value: breathing
            )
            .onAppear { if animate { breathing = true } }
    }
}

import SwiftUI

struct ReadyView: View {
    @EnvironmentObject var state: OnboardingState

    var body: some View {
        VStack(spacing: 24) {
            Spacer()

            LogoSquircle(progress: 1.0)

            VStack(spacing: 6) {
                Text("You're all set!")
                    .font(.system(size: 24, weight: .bold))
                    .foregroundStyle(Color.brandGreen)

                let names = state.selectedModels.map { backendDisplayName($0) }.sorted()
                Text(names.joined(separator: ", "))
                    .font(.callout)
                    .foregroundStyle(Color.brandGreen.opacity(0.6))
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 32)
            }

            VStack(spacing: 6) {
                HStack(spacing: 8) {
                    KeyCap("Control")
                    Image(systemName: "arrow.right")
                        .foregroundStyle(Color.brandGreen.opacity(0.45))
                    Text("speak")
                        .foregroundStyle(Color.brandGreen.opacity(0.85))
                    Image(systemName: "arrow.right")
                        .foregroundStyle(Color.brandGreen.opacity(0.45))
                    Text("release")
                        .foregroundStyle(Color.brandGreen.opacity(0.85))
                }
                .font(.system(size: 14))

                Text("Your words appear wherever your cursor is.")
                    .font(.caption)
                    .foregroundStyle(Color.brandGreen.opacity(0.55))
            }

            Toggle("Launch at Login", isOn: $state.autoStart)
                .toggleStyle(.switch)
                .tint(Color.brandCitron)
                .foregroundStyle(Color.brandGreen)
                .padding(.horizontal, 80)

            Spacer()

            Button(action: { state.finish() }) {
                Text("Start Whisper Push")
            }
            .buttonStyle(BrandPrimaryButtonStyle())
            .padding(.horizontal, 60)
            .padding(.bottom, 18)
        }
        .padding(.top, 24)
    }
}

/// Pill-shaped key cap reused in the Ready view to illustrate the hotkey
/// gesture. Brand-aligned: chamois cream fill + racing-green text/border —
/// no system grays.
struct KeyCap: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .font(.system(size: 12, weight: .semibold, design: .rounded))
            .foregroundStyle(Color.brandGreen)
            .padding(.horizontal, 10)
            .padding(.vertical, 4)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.brandCream)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .stroke(Color.brandGreen.opacity(0.2), lineWidth: 1)
            )
    }
}

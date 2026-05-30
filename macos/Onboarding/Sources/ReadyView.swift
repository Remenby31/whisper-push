import SwiftUI

struct ReadyView: View {
    @EnvironmentObject var state: OnboardingState

    var body: some View {
        VStack(spacing: 22) {
            Spacer()
            LogoSquircle()

            Text("You're all set")
                .font(.system(size: 24, weight: .bold))
                .foregroundStyle(Color.brandGreen)

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
            .padding(.bottom, 28)
        }
        .padding(.top, 22)
    }
}

/// Brand-aligned key cap. Chamois cream fill, racing green text.
struct KeyCap: View {
    let text: String
    init(_ text: String) { self.text = text }

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

import SwiftUI

struct WelcomeView: View {
    @EnvironmentObject var state: OnboardingState

    var body: some View {
        VStack(spacing: 22) {
            Spacer()

            LogoSquircle(animate: true)

            VStack(spacing: 6) {
                Text("Whisper Push")
                    .font(.system(size: 24, weight: .bold))
                    .foregroundStyle(Color.brandGreen)

                Text("Push to talk voice dictation")
                    .font(.callout)
                    .foregroundStyle(Color.brandGreen.opacity(0.6))
            }

            VStack(spacing: 10) {
                FeatureRow(icon: "lock.shield", text: "100% local. Nothing leaves your Mac.")
                FeatureRow(icon: "bolt.fill",   text: "GPU accelerated transcription.")
                FeatureRow(icon: "keyboard",    text: "Hold a key, speak, release.")
            }
            .padding(.horizontal, 40)

            Spacer()

            Button(action: { state.advance() }) {
                Text("Get Started")
            }
            .buttonStyle(BrandPrimaryButtonStyle())
            .padding(.horizontal, 60)
            .padding(.bottom, 14)
        }
        .padding(.top, 22)
    }
}

struct FeatureRow: View {
    let icon: String
    let text: String

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: icon)
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Color.brandGreen)
                .frame(width: 22, height: 22)
                .background(Circle().fill(Color.brandCitron.opacity(0.6)))
            Text(text)
                .font(.system(size: 13))
                .foregroundStyle(Color.brandGreen.opacity(0.85))
            Spacer()
        }
    }
}

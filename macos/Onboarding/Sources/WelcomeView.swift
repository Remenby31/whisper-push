import SwiftUI

struct WelcomeView: View {
    @EnvironmentObject var state: OnboardingState
    @State private var waveAnim = false

    var body: some View {
        VStack(spacing: 24) {
            Spacer()

            LogoSquircle(animate: waveAnim)
                .onAppear { waveAnim = true }

            VStack(spacing: 6) {
                Text("Whisper Push")
                    .font(.system(size: 24, weight: .bold))
                    .foregroundStyle(Color.brandGreen)

                Text("Push-to-talk voice dictation")
                    .font(.callout)
                    .foregroundStyle(Color.brandGreen.opacity(0.6))
            }

            VStack(spacing: 10) {
                FeatureRow(icon: "lock.shield", text: "100% local — nothing leaves your Mac")
                FeatureRow(icon: "bolt.fill", text: "GPU-accelerated transcription")
                FeatureRow(icon: "keyboard", text: "Hold a key, speak, release — text appears")
            }
            .padding(.horizontal, 40)

            Spacer()

            Button(action: { state.advance() }) {
                Text("Get Started")
            }
            .buttonStyle(BrandPrimaryButtonStyle())
            .padding(.horizontal, 60)
            .padding(.bottom, 18)
        }
        .padding(.top, 24)
    }
}

struct FeatureRow: View {
    let icon: String
    let text: String

    var body: some View {
        HStack(spacing: 12) {
            // Icon in a small citron badge for brand consistency — same
            // citron used on CTAs, but with low opacity so it doesn't
            // compete visually with the primary button.
            Image(systemName: icon)
                .font(.system(size: 13, weight: .medium))
                .foregroundColor(Color.brandGreen)
                .frame(width: 22, height: 22)
                .background(
                    Circle().fill(Color.brandCitron.opacity(0.6))
                )
            Text(text)
                .font(.system(size: 13))
                .foregroundStyle(Color.brandGreen.opacity(0.85))
            Spacer()
        }
    }
}

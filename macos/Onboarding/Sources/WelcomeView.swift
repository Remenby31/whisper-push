import SwiftUI

struct WelcomeView: View {
    @EnvironmentObject var state: OnboardingState
    @State private var waveAnim = false

    var body: some View {
        VStack(spacing: 24) {
            Spacer()

            LogoSquircle(animate: waveAnim)
                .onAppear { waveAnim = true }

            VStack(spacing: 8) {
                Text("Whisper Push")
                    .font(.system(size: 28, weight: .bold))

                Text("Push-to-talk voice dictation")
                    .font(.title3)
                    .foregroundStyle(.secondary)
            }

            VStack(spacing: 8) {
                FeatureRow(icon: "lock.shield", text: "100% local — nothing leaves your Mac")
                FeatureRow(icon: "bolt.fill", text: "GPU-accelerated transcription")
                FeatureRow(icon: "keyboard", text: "Hold a key, speak, release — text appears")
            }
            .padding(.horizontal, 40)

            Spacer()

            Button(action: { state.advance() }) {
                Text("Get Started")
                    .font(.headline)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 8)
            }
            .buttonStyle(.borderedProminent)
            .tint(Color.brandGreen)
            .controlSize(.large)
            .padding(.horizontal, 60)
            .padding(.bottom, 24)
        }
        .padding(.top, 32)
    }
}

struct FeatureRow: View {
    let icon: String
    let text: String

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: icon)
                .foregroundColor(.brandCitron)
                .frame(width: 20)
            Text(text)
                .font(.body)
            Spacer()
        }
    }
}

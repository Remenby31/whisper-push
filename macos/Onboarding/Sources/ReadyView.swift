import SwiftUI

struct ReadyView: View {
    @EnvironmentObject var state: OnboardingState

    var body: some View {
        VStack(spacing: 24) {
            Spacer()

            LogoSquircle(progress: 1.0)

            VStack(spacing: 8) {
                Text("You're all set!")
                    .font(.system(size: 28, weight: .bold))

                let names = state.selectedModels.map { backendDisplayName($0) }.sorted()
                Text(names.joined(separator: ", "))
                    .font(.body)
                    .foregroundStyle(.secondary)
            }

            VStack(spacing: 6) {
                HStack(spacing: 8) {
                    KeyCap("Control")
                    Image(systemName: "arrow.right")
                        .foregroundStyle(.tertiary)
                    Text("speak")
                        .foregroundStyle(.secondary)
                    Image(systemName: "arrow.right")
                        .foregroundStyle(.tertiary)
                    Text("release")
                        .foregroundStyle(.secondary)
                }
                .font(.body)

                Text("Your words appear wherever your cursor is.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }

            Toggle("Launch at Login", isOn: $state.autoStart)
                .toggleStyle(.switch)
                .padding(.horizontal, 80)

            Spacer()

            Button(action: { state.finish() }) {
                Text("Start Whisper Push")
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
        .padding(.top, 24)
    }
}

struct KeyCap: View {
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text)
            .font(.system(size: 13, weight: .medium, design: .rounded))
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.gray.opacity(0.12))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(Color.gray.opacity(0.25), lineWidth: 1)
            )
    }
}

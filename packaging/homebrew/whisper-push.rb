# Homebrew Cask for Whisper Push
# Usage (the fully-qualified name also trusts the cask: Homebrew 7 refuses
# untrusted third-party taps, so `brew tap` + a bare name fails):
#   brew install --cask remenby31/whisper-push/whisper-push
#   brew update && brew upgrade --cask whisper-push
#
# The tap repo (Remenby31/homebrew-whisper-push) should contain this file at:
#   Casks/whisper-push.rb

cask "whisper-push" do
  version "1.3.0"
  sha256 :no_check # Update with actual SHA after first release

  url "https://github.com/Remenby31/whisper-push/releases/download/v#{version}/Whisper-Push-macOS-arm64.dmg"
  name "Whisper Push"
  desc "Push-to-talk voice dictation, 100% local"
  homepage "https://github.com/Remenby31/whisper-push"

  depends_on arch: :arm64
  depends_on macos: :ventura

  app "Whisper Push.app"

  uninstall launchctl: "com.whisper-push.app",
            quit:      "com.whisper-push.app"

  zap trash: [
    "~/Library/Application Support/whisper-push",
    "~/Library/LaunchAgents/com.whisper-push.app.plist",
  ]

  caveats "Whisper Push downloads a speech model (~550 MB) on first launch."
end

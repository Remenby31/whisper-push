# Homebrew Cask for Whisper Push
# Usage:
#   brew tap Remenby31/whisper-push
#   brew install --cask whisper-push
#
# The tap repo (Remenby31/homebrew-whisper-push) should contain this file at:
#   Casks/whisper-push.rb

cask "whisper-push" do
  version "0.1.0"
  sha256 :no_check # Update with actual SHA after first release

  url "https://github.com/Remenby31/whisper-push/releases/download/v#{version}/Whisper-Push-macOS-arm64.dmg"
  name "Whisper Push"
  desc "Push-to-talk voice dictation, 100% local"
  homepage "https://github.com/Remenby31/whisper-push"

  depends_on macos: ">= :ventura"
  depends_on arch: :arm64

  app "Whisper Push.app"

  postflight do
    ohai "Whisper Push downloads a speech model (~550 MB) on first launch."
  end

  uninstall launchctl: "com.whisper-push.app",
            quit:      "com.whisper-push.app"

  zap trash: [
    "~/Library/Application Support/whisper-push",
    "~/Library/LaunchAgents/com.whisper-push.app.plist",
  ]
end

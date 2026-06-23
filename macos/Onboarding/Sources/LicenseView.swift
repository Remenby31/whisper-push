import SwiftUI
import AppKit

/// Paywall + activation step. Three modes in one modal:
///  • choose   — native plan cards (value prop, prices, badges) → one CTA
///  • checkout — embedded Lemon Squeezy payment (WKWebView), framed as "secure checkout"
///  • activate — key + email (auto-filled after purchase; manual fallback)
/// Reused as onboarding step 3/6 and standalone via `--license-only`
/// (menu bar → License → Subscription).
struct LicenseView: View {
    @EnvironmentObject var state: OnboardingState

    // Variant-locked, permanent checkout links (Lemon Squeezy). LIVE (prod) URLs.
    private let checkoutAnnual = "https://whisperpush.lemonsqueezy.com/checkout/buy/3b9fd0f0-f299-4108-86eb-93c03e2eca23"
    private let checkoutLifetime = "https://whisperpush.lemonsqueezy.com/checkout/buy/04ecf078-9a78-4daf-a5a5-edf77a019c07"

    // Strip the embedded checkout down to just the payment form. `embed=1` drops
    // the LS site chrome; `media/logo/desc/discount=0` remove the product image,
    // store logo, description and discount field — the block you used to scroll
    // past before reaching the card fields. (Lemon Squeezy checkout URL options.)
    private let checkoutOptions = "embed=1&media=0&logo=0&desc=0&discount=0"

    private enum Plan { case annual, lifetime }
    private enum Mode: Equatable { case choose, checkout(String), activate }

    @State private var mode: Mode = .choose
    @State private var plan: Plan = .annual
    @State private var key = ""
    @State private var email = ""
    @State private var busy = false
    @State private var message: String?
    @State private var activated = false

    var body: some View {
        Group {
            switch mode {
            case .choose: chooseView
            case .checkout(let url): checkoutView(url)
            case .activate: activateView
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.brandCream)
        // Grow the window only while the payment form is showing, so it fits
        // with no scroll; every other mode (plans, activate) stays compact.
        .onChange(of: mode) { _, newMode in
            if case .checkout = newMode {
                state.expandedForCheckout = true
            } else {
                state.expandedForCheckout = false
            }
        }
        .onDisappear { state.expandedForCheckout = false }
    }

    // MARK: Paywall

    private var chooseView: some View {
        VStack(spacing: 0) {
            Text("Unlock Whisper Push")
                .font(.system(size: 22, weight: .bold))
                .foregroundStyle(Color.brandGreen)
                .padding(.top, 26)

            Text("Unlimited dictation · every engine · up to 5 devices · 100% on-device")
                .font(.system(size: 12))
                .foregroundStyle(Color.brandGreen.opacity(0.65))
                .multilineTextAlignment(.center)
                .padding(.horizontal, 36)
                .padding(.top, 6)

            HStack(spacing: 12) {
                planCard(.annual, title: "Annual", price: "19,99 €", period: "per year", badge: "Most popular")
                planCard(.lifetime, title: "Lifetime", price: "49,99 €", period: "one-time", badge: "Best value")
            }
            .frame(maxWidth: 380)
            .padding(.top, 18)

            Button { mode = .checkout(plan == .annual ? checkoutAnnual : checkoutLifetime) } label: {
                Text("Continue")
            }
            .buttonStyle(BrandPrimaryButtonStyle(enabled: true))
            .padding(.horizontal, 70)
            .padding(.top, 18)

            Button("I already have a license key") { message = nil; mode = .activate }
                .buttonStyle(.plain)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Color.brandGreen.opacity(0.8))
                .padding(.top, 10)

            Spacer()
            trialLink.padding(.bottom, 18)
        }
    }

    private func planCard(_ p: Plan, title: String, price: String, period: String, badge: String) -> some View {
        let selected = plan == p
        return Button { plan = p } label: {
            VStack(spacing: 3) {
                Text(badge.uppercased())
                    .font(.system(size: 9, weight: .heavy))
                    .foregroundStyle(Color.brandGreen)
                    .padding(.horizontal, 8).padding(.vertical, 3)
                    .background(Capsule().fill(Color.brandCitron))
                Text(title)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Color.brandGreen)
                    .padding(.top, 6)
                Text(price)
                    .font(.system(size: 24, weight: .bold))
                    .foregroundStyle(Color.brandGreen)
                Text(period)
                    .font(.system(size: 11))
                    .foregroundStyle(Color.brandGreen.opacity(0.6))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 16)
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(selected ? Color.brandCitron.opacity(0.18) : Color.white)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .stroke(selected ? Color.brandGreen : Color.brandGreen.opacity(0.15),
                            lineWidth: selected ? 2 : 1)
            )
        }
        .buttonStyle(.plain)
        .animation(.easeOut(duration: 0.12), value: selected)
    }

    // MARK: Checkout (embedded payment)

    private func checkoutView(_ url: String) -> some View {
        VStack(spacing: 0) {
            HStack {
                Button { mode = .choose } label: { Label("Back", systemImage: "chevron.left").labelStyle(.titleAndIcon) }
                    .buttonStyle(.plain).font(.system(size: 12, weight: .semibold)).foregroundStyle(Color.brandGreen)
                Spacer()
                HStack(spacing: 4) {
                    Image(systemName: "lock.fill").font(.system(size: 10))
                    Text("Secure checkout").font(.system(size: 11, weight: .semibold))
                }
                .foregroundStyle(Color.brandGreen.opacity(0.7))
            }
            .padding(.horizontal, 16).padding(.top, 14).padding(.bottom, 8)

            // Minimal embedded checkout (see `checkoutOptions`). We poll the DOM
            // for the key (no Lemon.js needed).
            CheckoutView(url: URL(string: "\(url)?\(checkoutOptions)")!) { foundKey, foundEmail, success in
                if let foundKey { key = foundKey }
                if let foundEmail, email.isEmpty { email = foundEmail }
                let mail = foundEmail ?? (email.isEmpty ? nil : email)
                if !busy, !activated, let k = foundKey, let mail {
                    // Key + email captured → activate automatically, no copy/paste.
                    mode = .activate
                    runActivation(key: k, email: mail)
                } else if success {
                    message = key.isEmpty
                        ? "Payment received — paste the license key from your email."
                        : "Payment received — add your email to finish."
                    mode = .activate
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.brandGreen.opacity(0.12), lineWidth: 1))
            .padding(.horizontal, 14).padding(.bottom, 10)

            Button("Already paid? Enter your key →") { mode = .activate }
                .buttonStyle(.plain).font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Color.brandGreen.opacity(0.75))
                .padding(.bottom, 12)
        }
    }

    // MARK: Activate (auto after purchase, or manual fallback)

    private var activateView: some View {
        VStack(spacing: 0) {
            LogoSquircle(size: 52).padding(.top, 24)
            Text(activated ? "You're all set" : "Activate")
                .font(.system(size: 22, weight: .bold)).foregroundStyle(Color.brandGreen).padding(.top, 10)

            if !activated {
                VStack(spacing: 8) {
                    TextField("License key", text: $key).textFieldStyle(.roundedBorder).disableAutocorrection(true)
                    TextField("Purchase email", text: $email).textFieldStyle(.roundedBorder).disableAutocorrection(true)
                }
                .frame(maxWidth: 320)
                .padding(.top, 16)
            }

            if let message {
                Text(message).font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Color.brandGreen).multilineTextAlignment(.center)
                    .padding(.horizontal, 36).padding(.top, 14)
            }

            if !activated {
                Button(action: activate) { Text(busy ? "Activating…" : "Activate") }
                    .buttonStyle(BrandPrimaryButtonStyle(enabled: canActivate)).disabled(!canActivate)
                    .padding(.horizontal, 80).padding(.top, 16)
                Button("‹ Back to plans") { message = nil; mode = .choose }
                    .buttonStyle(.plain).font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Color.brandGreen.opacity(0.8)).padding(.top, 10)
            }

            Spacer()
            (activated ? AnyView(doneButton) : AnyView(trialLink)).padding(.bottom, 18)
        }
    }

    // MARK: Bottom actions

    private var trialLink: some View {
        Button(action: proceed) {
            Text(state.licenseOnly ? "Close" : "Start 7-day free trial")
        }
        .buttonStyle(.plain)
        .font(.system(size: 13, weight: .semibold))
        .foregroundStyle(Color.brandGreen.opacity(0.85))
    }

    private var doneButton: some View {
        Button(action: proceed) { Text(state.licenseOnly ? "Done" : "Continue") }
            .buttonStyle(BrandPrimaryButtonStyle(enabled: true))
            .padding(.horizontal, 60)
    }

    // MARK: Actions

    private var canActivate: Bool {
        !busy && !key.trimmingCharacters(in: .whitespaces).isEmpty && email.contains("@")
    }

    private func proceed() {
        if state.licenseOnly { NSApplication.shared.terminate(nil) } else { state.advance() }
    }

    private func activate() {
        runActivation(key: key.trimmingCharacters(in: .whitespaces),
                      email: email.trimmingCharacters(in: .whitespaces))
    }

    /// Shared activation core — used by the manual button and by auto-activation
    /// after an in-app purchase.
    private func runActivation(key k: String, email e: String) {
        guard !busy, let path = state.daemonPath,
              FileManager.default.isExecutableFile(atPath: path) else {
            message = "Activation needs the installed app."
            return
        }
        key = k; email = e // reflect captured values in the fields
        busy = true; message = nil
        DispatchQueue.global().async {
            let (ok, err) = Self.runActivate(path: path, key: k, email: e)
            DispatchQueue.main.async {
                busy = false; activated = ok
                message = ok ? "Your license is active — thank you!"
                             : (err ?? "Couldn't activate — check the key and email above.")
            }
        }
    }

    /// Run `daemon license activate …`, parse the final JSON line.
    private static func runActivate(path: String, key: String, email: String) -> (Bool, String?) {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = ["license", "activate", "--key", key, "--email", email]
        let out = Pipe(); p.standardOutput = out; p.standardError = Pipe()
        do { try p.run() } catch { return (false, "Couldn't start activation.") }
        p.waitUntilExit()
        let data = out.fileHandleForReading.readDataToEndOfFile()
        let line = String(data: data, encoding: .utf8)?.split(separator: "\n").last.map(String.init) ?? ""
        guard let obj = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any] else {
            return (false, nil)
        }
        if (obj["activated"] as? Bool) ?? false { return (true, nil) }
        let err = obj["error"] as? String
        return (false, err == "offline" ? "No connection — check your internet and retry." : err)
    }
}

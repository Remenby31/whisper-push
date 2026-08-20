import SwiftUI
import AppKit

/// Paywall + activation step. Four modes in one modal:
///  • choose   — native plan cards (value prop, prices, badges) → one CTA
///  • checkout — embedded Lemon Squeezy payment (WKWebView), framed as "secure checkout"
///  • activate — license key only (auto-filled after purchase or from the
///               clipboard; typed/pasted as the fallback)
///  • licensed — what an already-licensed user sees instead of the paywall
///               (plan, purchase email, deactivate)
/// Reused as onboarding step 3/6 and standalone via `--license-only`
/// (menu bar → License → Subscribe… / Manage License… / Enter License Key…).
struct LicenseView: View {
    @EnvironmentObject var state: OnboardingState

    // Variant-locked, permanent checkout links (Lemon Squeezy). LIVE (prod) URLs.
    // Two separate products now: Monthly (€4.99/mo) and Lifetime (€49.99 one-time).
    private let checkoutMonthly = "https://whisperpush.lemonsqueezy.com/checkout/buy/2baac143-5393-465e-8d0c-66ee9bd12ab3"
    private let checkoutLifetime = "https://whisperpush.lemonsqueezy.com/checkout/buy/04ecf078-9a78-4daf-a5a5-edf77a019c07"

    // Strip the embedded checkout down to just the payment form. `embed=1` drops
    // the LS site chrome; `media/logo/desc=0` remove the product image, store logo
    // and description — the block you used to scroll past before reaching the card
    // fields. `discount=1` keeps the "Add discount code" field visible so promo
    // codes can be redeemed in-app. (Lemon Squeezy checkout URL options.)
    private let checkoutOptions = "embed=1&media=0&logo=0&desc=0&discount=1"

    private enum Plan { case monthly, lifetime }
    private enum Mode: Equatable { case choose, checkout(String), activate, licensed }

    /// User-driven screen. `nil` = "whatever the launch state implies" (see
    /// `mode`), so the first frame is already the right screen: resolving it
    /// asynchronously after appearing made the paywall flash before the licensed
    /// screen, and that view swap could fire the incoming button's action by
    /// itself — the modal closed a few seconds after opening.
    @State private var pickedMode: Mode?
    @State private var plan: Plan = .monthly
    @State private var key = ""
    @State private var busy = false
    @State private var message: String?
    @State private var activated = false
    /// Set only after an activation/deactivation in this window; otherwise the
    /// snapshot taken at launch is the truth.
    @State private var refreshed: LicenseSnapshot??
    @FocusState private var keyFocused: Bool
    @State private var revealKey = false

    /// The screen to show: the user's pick, else what the license implies.
    private var mode: Mode {
        if let pickedMode { return pickedMode }
        if state.startActivate { return .activate }
        return info?.isLicensed == true ? .licensed : .choose
    }

    /// Current license: the post-action refresh if one happened, else launch.
    private var info: LicenseSnapshot? {
        if case let .some(v) = refreshed { return v }
        return state.license
    }

    var body: some View {
        Group {
            switch mode {
            case .choose: chooseView
            case .checkout(let url): checkoutView(url)
            case .activate: activateView
            case .licensed: licensedView
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
            if newMode == .activate { prefillFromClipboard() }
        }
        .onDisappear { state.expandedForCheckout = false }
        // No license lookup here: `state.license` was read before this window
        // existed, so `mode` is already correct on the first frame — a licensed
        // user never sees the paywall flash.
        .onAppear { if mode == .activate { prefillFromClipboard() } }
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
                planCard(.monthly, title: "Monthly", price: "4,99 €", period: "per month", badge: "Flexible")
                planCard(.lifetime, title: "Lifetime", price: "49,99 €", period: "one-time", badge: "Best value")
            }
            .frame(maxWidth: 380)
            .padding(.top, 18)

            Button { pickedMode = .checkout(plan == .monthly ? checkoutMonthly : checkoutLifetime) } label: {
                Text("Continue")
            }
            .buttonStyle(BrandPrimaryButtonStyle(enabled: true))
            .padding(.horizontal, 70)
            .padding(.top, 18)

            Button("I already have a license key") { message = nil; pickedMode = .activate }
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
                Button { pickedMode = .choose } label: { Label("Back", systemImage: "chevron.left").labelStyle(.titleAndIcon) }
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
            // for the key (no Lemon.js needed). The email LS shows alongside is
            // not needed — the key alone activates.
            CheckoutView(url: URL(string: "\(url)?\(checkoutOptions)")!) { foundKey, _, success in
                if let foundKey, !busy, !activated {
                    // Key captured → activate automatically, no copy/paste.
                    pickedMode = .activate
                    runActivation(key: foundKey)
                } else if success, !activated {
                    message = "Payment received — paste the license key from your email."
                    pickedMode = .activate
                }
            }
            .clipShape(RoundedRectangle(cornerRadius: 10))
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.brandGreen.opacity(0.12), lineWidth: 1))
            .padding(.horizontal, 14).padding(.bottom, 10)

            Button("Already paid? Enter your key →") { pickedMode = .activate }
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
                Text("Paste the license key from your purchase email.")
                    .font(.system(size: 12))
                    .foregroundStyle(Color.brandGreen.opacity(0.65))
                    .padding(.top, 4)

                HStack(spacing: 6) {
                    TextField("XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX", text: $key)
                        .textFieldStyle(.roundedBorder)
                        .font(.system(size: 12, design: .monospaced))
                        .disableAutocorrection(true)
                        .focused($keyFocused)
                        .onSubmit { if canActivate { activate() } }
                    // One-click paste: works even when keyboard focus is flaky
                    // (the modal is spawned by a background agent and macOS may
                    // refuse to make it the active app).
                    IconButton(glyph: .clipboardPaste, help: "Paste from clipboard") {
                        if let s = Self.clipboardKey(strict: false) { key = s }
                    }
                }
                .frame(maxWidth: 340)
                .padding(.top, 14)
                // Focus the field as soon as the screen is up so typing/⌘V just works.
                .onAppear { DispatchQueue.main.async { keyFocused = true } }
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
                Button("Buy a license") { message = nil; pickedMode = .choose }
                    .buttonStyle(.plain).font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Color.brandGreen.opacity(0.8)).padding(.top, 10)
            }

            Spacer()
            (activated ? AnyView(doneButton) : AnyView(trialLink)).padding(.bottom, 18)
        }
    }

    // MARK: Licensed (manage)

    private var licensedView: some View {
        VStack(spacing: 0) {
            LogoSquircle(size: 52).padding(.top, 24)
            Text("License active")
                .font(.system(size: 22, weight: .bold)).foregroundStyle(Color.brandGreen).padding(.top, 10)

            VStack(spacing: 6) {
                Text(planLabel)
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Color.brandGreen)
                if let email = info?.email, !email.isEmpty {
                    Text("Licensed to \(email)")
                        .font(.system(size: 12))
                        .foregroundStyle(Color.brandGreen.opacity(0.65))
                }
            }
            .multilineTextAlignment(.center)
            .padding(.horizontal, 36)
            .padding(.top, 12)

            // The key itself, so activating a second device doesn't mean digging
            // through the purchase email. Masked by default — it's the one thing
            // on this screen worth hiding from a screen-share or a screenshot;
            // click it to reveal, or copy it without ever showing it.
            if let k = info?.key, !k.isEmpty {
                HStack(spacing: 2) {
                    Text(revealKey ? k : Self.masked(k))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.brandGreen.opacity(0.75))
                        .textSelection(.enabled)
                        .onTapGesture { revealKey.toggle() }
                        .help(revealKey ? "Click to hide" : "Click to reveal")
                    IconButton(glyph: .copy, size: 14, help: "Copy license key") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(k, forType: .string)
                        message = "License key copied."
                    }
                }
                .padding(.top, 12)
            }

            if let message {
                Text(message).font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Color.brandGreen).multilineTextAlignment(.center)
                    .padding(.horizontal, 36).padding(.top, 14)
            }

            // Self-serve billing: Lemon Squeezy's customer portal covers invoices,
            // payment method and cancellation, and lists the license keys. It
            // signs the customer in with an emailed magic link, so all they need
            // is the address shown above — not the original purchase email.
            Button { openBillingPortal() } label: {
                Text(info?.kind == "subscription"
                     ? "Manage subscription & invoices \u{2197}"
                     : "Invoices & purchase details \u{2197}")
            }
            .buttonStyle(.plain)
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(Color.brandGreen)
            .padding(.top, 18)
            .help("Opens \(Self.billingPortal) in your browser")

            Button(action: deactivate) { Text(busy ? "Deactivating…" : "Deactivate this device") }
                .buttonStyle(.plain).font(.system(size: 12))
                .foregroundStyle(Color.brandGreen.opacity(0.55))
                .disabled(busy)
                .padding(.top, 10)
                .help("Frees one of your device slots; you can re-activate anytime.")

            Spacer()
            doneButton.padding(.bottom, 18)
        }
    }

    /// Lemon Squeezy's hosted customer portal for this store.
    static let billingPortal = "https://whisperpush.lemonsqueezy.com/billing"

    private func openBillingPortal() {
        if let url = URL(string: Self.billingPortal) { NSWorkspace.shared.open(url) }
    }

    /// `C281261E-••••-••••-••••-••••••••C7C4` — enough to recognise which key it
    /// is, not enough to use it.
    static func masked(_ key: String) -> String {
        guard key.count > 12 else { return String(repeating: "\u{2022}", count: key.count) }
        let head = key.prefix(8), tail = key.suffix(4)
        return "\(head)-\u{2022}\u{2022}\u{2022}\u{2022}-\u{2022}\u{2022}\u{2022}\u{2022}-\u{2022}\u{2022}\u{2022}\u{2022}-\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\(tail)"
    }

    private var planLabel: String {
        switch info?.kind {
        case "lifetime": return "Lifetime — paid once, yours forever"
        case "subscription":
            if let d = info?.renews, !d.isEmpty { return "Monthly — renews \(d)" }
            return "Monthly subscription"
        default: return "Whisper Push"
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
        !busy && !key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func proceed() {
        if state.licenseOnly { NSApplication.shared.terminate(nil) } else { state.advance() }
    }

    private func activate() {
        runActivation(key: key.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    /// If the clipboard holds something that looks like a Lemon Squeezy key
    /// (UUID), drop it into the field — the user just copied it from the email.
    private func prefillFromClipboard() {
        if key.isEmpty, let s = Self.clipboardKey(strict: true) { key = s }
    }

    /// Clipboard text as a candidate key. `strict` requires the UUID shape (used
    /// for silent prefill); the paste button accepts any single-line text.
    static func clipboardKey(strict: Bool) -> String? {
        guard let raw = NSPasteboard.general.string(forType: .string) else { return nil }
        let s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty, !s.contains("\n") else { return nil }
        if !strict { return s }
        let uuid = "^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$"
        return s.range(of: uuid, options: .regularExpression) != nil ? s : nil
    }

    /// Shared activation core — used by the manual button and by auto-activation
    /// after an in-app purchase.
    private func runActivation(key k: String) {
        guard !busy, let path = daemonBinary else {
            message = "Activation needs the installed app."
            return
        }
        key = k // reflect the captured value in the field
        busy = true; message = nil
        DispatchQueue.global().async {
            let (ok, err) = Self.runDaemon(path: path, ["license", "activate", "--key", k], successKey: "activated")
            DispatchQueue.main.async {
                busy = false; activated = ok
                message = ok ? "Your license is active — thank you!"
                             : (err ?? "Couldn't activate — check the key above.")
                if ok { refreshLicenseInfo() }
            }
        }
    }

    private func deactivate() {
        guard !busy, let path = daemonBinary else { return }
        busy = true; message = nil
        DispatchQueue.global().async {
            let (ok, _) = Self.runDaemon(path: path, ["license", "deactivate"], successKey: "result", successValue: "done")
            DispatchQueue.main.async {
                busy = false
                if ok {
                    refreshed = .some(nil) // no license any more
                    message = "This device has been deactivated."
                    pickedMode = .choose
                } else {
                    message = "Couldn't reach the server — check your connection and retry."
                }
            }
        }
    }

    /// Re-read the license after an action of ours changed it (activation).
    /// Never called on appear — the launch snapshot covers that, and swapping
    /// the view under the user is what made the window close itself.
    private func refreshLicenseInfo() {
        guard let path = daemonBinary else { return }
        DispatchQueue.global().async {
            let obj = Self.runDaemonJSON(path: path, ["license", "status"])
            let snap = (obj?["status"] as? String).map { st in
                LicenseSnapshot(status: st,
                                kind: obj?["kind"] as? String ?? "",
                                email: obj?["email"] as? String,
                                renews: obj?["renews"] as? String)
            }
            DispatchQueue.main.async { refreshed = .some(snap) }
        }
    }

    private var daemonBinary: String? {
        guard let path = state.daemonPath, FileManager.default.isExecutableFile(atPath: path) else { return nil }
        return path
    }

    /// Run the daemon CLI and parse its final JSON line.
    private static func runDaemonJSON(path: String, _ args: [String]) -> [String: Any]? {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: path)
        p.arguments = args
        let out = Pipe(); p.standardOutput = out; p.standardError = Pipe()
        do { try p.run() } catch { return nil }
        p.waitUntilExit()
        let data = out.fileHandleForReading.readDataToEndOfFile()
        let line = String(data: data, encoding: .utf8)?.split(separator: "\n").last.map(String.init) ?? ""
        return try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any]
    }

    /// Run a daemon command and report (success, humanized error).
    private static func runDaemon(path: String, _ args: [String],
                                  successKey: String, successValue: String? = nil) -> (Bool, String?) {
        guard let obj = runDaemonJSON(path: path, args) else {
            return (false, "Couldn't start the activation helper.")
        }
        let ok: Bool
        if let want = successValue {
            ok = (obj[successKey] as? String) == want
        } else {
            ok = (obj[successKey] as? Bool) ?? false
        }
        if ok { return (true, nil) }
        let err = obj["error"] as? String
        return (false, err == "offline" ? "No connection — check your internet and retry." : err)
    }
}

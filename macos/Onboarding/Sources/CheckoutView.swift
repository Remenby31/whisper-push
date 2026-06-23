import SwiftUI
import WebKit

/// Embedded Lemon Squeezy checkout (WKWebView) — the user pays inside the modal,
/// no external browser. After payment the LS confirmation view shows the license
/// key; we poll the DOM (it renders client-side, often without a full navigation)
/// and report the key + email back so activation can happen automatically.
struct CheckoutView: NSViewRepresentable {
    let url: URL
    /// (key?, email?, paymentLooksComplete) — called repeatedly as the page evolves.
    let onResult: (String?, String?, Bool) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(onResult: onResult) }

    func makeNSView(context: Context) -> WKWebView {
        let cfg = WKWebViewConfiguration()
        cfg.userContentController.add(context.coordinator, name: "lemon")
        // Belt-and-suspenders to the URL options: zero the top margin/padding LS
        // reserves so the form sits flush at the top — no dead space to scroll
        // past. CSS-only and scoped to the page root, so it can't break the form.
        let trimTop = """
        var s = document.createElement('style');
        s.textContent = 'html,body{margin:0 !important;padding-top:0 !important;}';
        document.documentElement.appendChild(s);
        """
        cfg.userContentController.addUserScript(
            WKUserScript(source: trimTop, injectionTime: .atDocumentEnd, forMainFrameOnly: true)
        )
        let web = WKWebView(frame: .zero, configuration: cfg)
        web.navigationDelegate = context.coordinator
        context.coordinator.web = web
        web.load(URLRequest(url: url))
        context.coordinator.startPolling()
        return web
    }

    func updateNSView(_ web: WKWebView, context: Context) {}

    static func dismantleNSView(_ web: WKWebView, coordinator: Coordinator) {
        coordinator.timer?.invalidate()
        web.configuration.userContentController.removeScriptMessageHandler(forName: "lemon")
    }

    final class Coordinator: NSObject, WKNavigationDelegate, WKScriptMessageHandler {
        let onResult: (String?, String?, Bool) -> Void
        weak var web: WKWebView?
        var timer: Timer?
        private var done = false

        init(onResult: @escaping (String?, String?, Bool) -> Void) { self.onResult = onResult }

        /// The confirmation page (and its license key) renders after payment with
        /// no full navigation, so poll the DOM until we capture a key.
        func startPolling() {
            timer = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in
                guard let self, !self.done, let web = self.web else { return }
                self.scan(web)
            }
        }

        func webView(_ web: WKWebView, didFinish _: WKNavigation!) { scan(web) }

        private func scan(_ web: WKWebView) {
            let js = #"""
            (function(){
              var t = document.body ? document.body.innerText : "";
              var k = (t.match(/[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/)||[])[0] || "";
              var e = (t.match(/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/)||[])[0] || "";
              var s = /thank you|confirm|success|receipt|your order|order #|license key/i.test(location.href + " " + t);
              window.webkit.messageHandlers.lemon.postMessage({key:k, email:e, success:s});
            })();
            """#
            web.evaluateJavaScript(js, completionHandler: nil)
        }

        func userContentController(_: WKUserContentController, didReceive msg: WKScriptMessage) {
            guard let d = msg.body as? [String: Any] else { return }
            let clean: (Any?) -> String? = { ($0 as? String).flatMap { $0.isEmpty ? nil : $0 } }
            let key = clean(d["key"])
            if key != nil { done = true; timer?.invalidate() } // stop once captured
            onResult(key, clean(d["email"]), (d["success"] as? Bool) ?? false)
        }
    }
}

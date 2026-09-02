//! The one HTTP client the app uses — model downloads, license calls, update
//! checks, enrichment.
//!
//! It exists for one reason: **TLS root certificates**. ureq's default is
//! `RootCerts::WebPki`, Mozilla's bundled list, which is the right choice for a
//! server that wants to be un-MITM-able. It is the wrong choice for a desktop
//! app on a managed machine: corporate networks routinely intercept TLS
//! (Zscaler, Netskope, a proxy appliance), presenting a certificate signed by a
//! private root that IS installed in the OS trust store and is NOT in Mozilla's
//! list. With the bundled roots, every HTTPS call fails on such a machine —
//! the model download, activating a license, checking for updates — with an
//! opaque certificate error, and the app looks broken through no fault of the
//! user's. The platform verifier trusts exactly what the rest of the machine
//! trusts, which is the only defensible answer for software someone installs on
//! a work laptop.
//!
//! Proxies come along for free: ureq's default config already reads
//! `HTTPS_PROXY` / `HTTP_PROXY` / `ALL_PROXY` from the environment.
//!
//! Everything else here is just so the four call sites stop each rebuilding the
//! same agent with the same User-Agent.

use std::sync::OnceLock;
use std::time::Duration;

/// Shared agent — connection pooling included, which matters for the license
/// calls that fire on a timer.
static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

/// How we identify ourselves. GitHub's API rejects requests without one.
pub fn user_agent() -> String {
    format!("whisper-push/{}", env!("CARGO_PKG_VERSION"))
}

/// The app's HTTP agent.
pub fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        let tls = ureq::tls::TlsConfig::builder()
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .user_agent(user_agent())
            .build()
            .new_agent()
    })
}

/// A GET with a total deadline. Every network call in the app is bounded: a
/// half-open socket must eventually surface as an error rather than wedge the
/// thread that is waiting on it (the pipeline thread, in the download case).
pub fn get(url: &str, timeout: Duration) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
    agent()
        .get(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
}

/// A POST with a total deadline, for the license API.
pub fn post(url: &str, timeout: Duration) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    agent()
        .post(url)
        .config()
        .timeout_global(Some(timeout))
        .build()
}

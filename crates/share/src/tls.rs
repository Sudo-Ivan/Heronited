//! TLS setup shared by the HTTP-ish backends: extra roots from a PEM file,
//! or verification off where the user asked for it.

use serde::{Deserialize, Serialize};

use crate::error::{Result, ShareError};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TlsOptions {
    /// Extra PEM roots (a self-signed CA or server certificate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,
    /// Accept any certificate. Clearly wrong outside a LAN, so the UI warns.
    #[serde(default)]
    pub insecure: bool,
}

/// ring, named explicitly: the process often has aws-lc-rs in the tree too
/// (ureq), and then rustls refuses to guess a default.
macro_rules! builder {
    () => {
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("ring supports the default protocol versions")
    };
}

impl TlsOptions {
    /// ureq's TLS configuration for these options.
    pub fn tls_config(&self) -> Result<ureq::tls::TlsConfig> {
        let mut roots: Vec<ureq::tls::Certificate<'static>> = Vec::new();
        if let Some(ca) = &self.ca_file {
            let pem = std::fs::read(ca).map_err(|e| {
                ShareError::config(heronite_i18n::tr!("The CA file {file} does not read: {error}", file = ca, error = e.to_string()))
            })?;
            let mut found = 0;
            for item in ureq::tls::parse_pem(&pem) {
                if let Ok(ureq::tls::PemItem::Certificate(c)) = item {
                    roots.push(c);
                    found += 1;
                }
            }
            if found == 0 {
                return Err(ShareError::config(heronite_i18n::tr!("The CA file {file} has no certificate in it", file = ca)));
            }
        }
        let root_certs = if roots.is_empty() { ureq::tls::RootCerts::WebPki } else { ureq::tls::RootCerts::new_with_certs(&roots) };
        Ok(ureq::tls::TlsConfig::builder().root_certs(root_certs).disable_verification(self.insecure).build())
    }

    /// rustls client configuration for the backends that speak TLS directly
    /// (FTP). WebPki roots plus the CA file, or verification off.
    pub fn rustls_config(&self) -> Result<std::sync::Arc<rustls::ClientConfig>> {
        use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
        use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
        use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};

        if self.insecure {
            #[derive(Debug)]
            struct AcceptAll;
            impl ServerCertVerifier for AcceptAll {
                fn verify_server_cert(
                    &self,
                    _: &CertificateDer<'_>,
                    _: &[CertificateDer<'_>],
                    _: &ServerName<'_>,
                    _: &[u8],
                    _: UnixTime,
                ) -> std::result::Result<ServerCertVerified, TlsError> {
                    Ok(ServerCertVerified::assertion())
                }
                fn verify_tls12_signature(
                    &self,
                    _: &[u8],
                    _: &CertificateDer<'_>,
                    _: &DigitallySignedStruct,
                ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
                    Ok(HandshakeSignatureValid::assertion())
                }
                fn verify_tls13_signature(
                    &self,
                    _: &[u8],
                    _: &CertificateDer<'_>,
                    _: &DigitallySignedStruct,
                ) -> std::result::Result<HandshakeSignatureValid, TlsError> {
                    Ok(HandshakeSignatureValid::assertion())
                }
                fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
                    vec![
                        SignatureScheme::RSA_PKCS1_SHA256,
                        SignatureScheme::RSA_PKCS1_SHA384,
                        SignatureScheme::RSA_PKCS1_SHA512,
                        SignatureScheme::ECDSA_NISTP256_SHA256,
                        SignatureScheme::ECDSA_NISTP384_SHA384,
                        SignatureScheme::RSA_PSS_SHA256,
                        SignatureScheme::RSA_PSS_SHA384,
                        SignatureScheme::RSA_PSS_SHA512,
                        SignatureScheme::ED25519,
                    ]
                }
            }
            let config = builder!().dangerous().with_custom_certificate_verifier(std::sync::Arc::new(AcceptAll)).with_no_client_auth();
            return Ok(std::sync::Arc::new(config));
        }
        let mut store = rustls::RootCertStore::empty();
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Some(ca) = &self.ca_file {
            let pem = std::fs::read(ca).map_err(|e| {
                ShareError::config(heronite_i18n::tr!("The CA file {file} does not read: {error}", file = ca, error = e.to_string()))
            })?;
            let mut found = 0;
            for cert in rustls_pemfile::certs(&mut &pem[..]).flatten() {
                store.add(cert).map_err(|e| ShareError::config(format!("CA file: {e}")))?;
                found += 1;
            }
            if found == 0 {
                return Err(ShareError::config(heronite_i18n::tr!("The CA file {file} has no certificate in it", file = ca)));
            }
        }
        let config = builder!().with_root_certificates(store).with_no_client_auth();
        Ok(std::sync::Arc::new(config))
    }
}

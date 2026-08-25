//! Minimal PEM parsing for TLS material, replacing the `rustls-pemfile`
//! crate. Mirrors its two entry points (`certs`, `private_key`) so call
//! sites only change their import path.

use std::io::{self, BufRead};

use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
};

/// Every `CERTIFICATE` section in the file, decoded to DER.
pub fn certs(reader: &mut dyn BufRead) -> std::vec::IntoIter<io::Result<CertificateDer<'static>>> {
    let results: Vec<io::Result<CertificateDer<'static>>> = sections(reader)
        .into_iter()
        .map(|(label, der)| match label.as_str() {
            "CERTIFICATE" => Ok(CertificateDer::from(der)),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected a CERTIFICATE section, found {other}"),
            )),
        })
        .collect();
    results.into_iter()
}

/// The first private-key section (PKCS#8, PKCS#1, or SEC1), decoded to DER.
pub fn private_key(
    reader: &mut dyn BufRead,
) -> io::Result<Option<PrivateKeyDer<'static>>> {
    for (label, der) in sections(reader) {
        let key = match label.as_str() {
            "PRIVATE KEY" => PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der)),
            "RSA PRIVATE KEY" => PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der)),
            "EC PRIVATE KEY" => PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der)),
            _ => continue,
        };
        return Ok(Some(key));
    }
    Ok(None)
}

/// Extracts every PEM section as `(label, base64-decoded DER)` pairs.
fn sections(reader: &mut dyn BufRead) -> Vec<(String, Vec<u8>)> {
    let mut text = String::new();
    if reader.read_to_string(&mut text).is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    while let Some(line) = lines.next() {
        let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|rest| rest.strip_suffix("-----"))
        else {
            continue;
        };
        // Accumulate base64 until the matching END marker; anything else is
        // malformed and yields an empty (invalid) section body.
        let mut body = String::new();
        let mut closed = false;
        for inner in lines.by_ref() {
            if inner == &format!("-----END {label}-----") {
                closed = true;
                break;
            }
            body.push_str(inner);
        }
        if closed && let Some(der) = super::base64::decode(&body) {
            out.push((label.to_owned(), der));
        }
    }
    out
}

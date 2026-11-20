// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jon Andrew
// Part of Penny, CuBit's web browser. Not derived from Servo's servoshell.

//! Read-only inspector of the active document's actual rustls handshake.
//! Penny's SPARK provider (penny-crypto) did the cryptography; parsing here does
//! not establish trust or make a second connection. rustls remains the sole verifier.
// The certificate summary (subject and issuer common names, validity),
// read from the DER by this small strict walker: no C X.509 parser
// (WEB-004; aws-lc left Penny). Anything unexpected is "Unavailable".
use servo::WebView;

/// One DER element at the start of `input`: (tag, contents, rest).
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = match first {
        0..=0x7f => (first as usize, rest),
        0x81..=0x83 => {
            let count = (first & 0x7f) as usize;
            if rest.len() < count { return None; }
            (rest[..count].iter().fold(0usize, |a, &b| (a << 8) | b as usize), &rest[count..])
        },
        _ => return None,
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const VERSION: u8 = 0xa0;
const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];

struct Summary<'a> {
    issuer: &'a [u8],
    not_before: (u8, &'a [u8]),
    not_after: (u8, &'a [u8]),
    subject: &'a [u8],
}

fn summary(der: &[u8]) -> Option<Summary<'_>> {
    let (SEQUENCE, certificate, _) = element(der)? else { return None };
    let (SEQUENCE, tbs, _) = element(certificate)? else { return None };
    let mut rest = tbs;
    let (tag, _, after) = element(rest)?;
    if tag == VERSION { rest = after; }
    let (_serial, _, rest) = element(rest)?;
    let (_algorithm, _, rest) = element(rest)?;
    let (SEQUENCE, issuer, rest) = element(rest)? else { return None };
    let (SEQUENCE, validity, rest) = element(rest)? else { return None };
    let (SEQUENCE, subject, _) = element(rest)? else { return None };
    let (before_tag, before, after) = element(validity)?;
    let (after_tag, not_after, _) = element(after)?;
    Some(Summary { issuer, not_before: (before_tag, before), not_after: (after_tag, not_after), subject })
}

fn common_name(name: &[u8]) -> String {
    let mut sets = name;
    while let Some((SET, set, rest)) = element(sets) {
        sets = rest;
        let Some((SEQUENCE, pair, _)) = element(set) else { continue };
        let Some((OID, oid, value)) = element(pair) else { continue };
        if oid == COMMON_NAME {
            if let Some((_, text, _)) = element(value) {
                return String::from_utf8_lossy(text).into_owned();
            }
        }
    }
    "No common name (identity may use subjectAltName)".into()
}

/// UTCTime (YYMMDDHHMMSSZ, RFC 5280: 50..99 is 19xx) or GeneralizedTime.
fn date((tag, text): (u8, &[u8])) -> String {
    let text = std::str::from_utf8(text).unwrap_or("");
    let digits = text.strip_suffix('Z').unwrap_or("");
    let full = match (tag, digits.len()) {
        (UTC_TIME, 12) => {
            let year: u32 = digits[..2].parse().unwrap_or(0);
            format!("{}{digits}", if year >= 50 { "19" } else { "20" })
        },
        (GENERALIZED_TIME, 14) => digits.to_string(),
        _ => return "Unavailable".into(),
    };
    if !full.bytes().all(|b| b.is_ascii_digit()) { return "Unavailable".into(); }
    format!("{}-{}-{} {}:{}:{} UTC", &full[..4], &full[4..6], &full[6..8], &full[8..10], &full[10..12], &full[12..14])
}

fn line(out: &mut String, text: &str) {
    // Plain ASCII, no control characters from remote certificate names.
    let safe: String = text.chars().map(|c| if c.is_ascii() && !c.is_ascii_control() {c} else {'?'}).collect();
    if safe.is_empty() { out.push('\n'); }
    for part in safe.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(part).unwrap()); out.push('\n');
    }
}
fn certificate(out: &mut String, der: &[u8], index: usize) {
    line(out, &format!("Certificate {}{}", index+1, if index==0 {" (site)"} else {" (issuer)"}));
    if der.is_empty() || der.len() > 65536 { line(out, "Certificate exceeds inspector limit."); return; }
    let Some(cert) = summary(der) else { line(out, "Certificate details could not be decoded."); return; };
    line(out, &format!("Subject: {}", common_name(cert.subject)));
    line(out, &format!("Issuer:  {}", common_name(cert.issuer)));
    line(out, &format!("Valid from: {}", date(cert.not_before)));
    line(out, &format!("Valid until: {}", date(cert.not_after)));
    let hash = penny_crypto::sha256(der).unwrap_or_default();
    let hex: String = hash.iter().map(|b| format!("{b:02X}")).collect();
    line(out, "SHA-256 fingerprint:"); line(out, &hex); line(out, "");
}
pub fn report(view: &WebView, loading: bool) -> String {
    let mut out = String::new();
    let Some(url) = view.url() else { return "No document loaded.\n".into(); };
    line(&mut out, &format!("Site: {}", url.host_str().unwrap_or("Local document")));
    if loading { line(&mut out, "Navigation in progress; connection details pending."); return out; }
    if url.scheme() != "https" {
        line(&mut out, if url.scheme()=="http" {"HTTP: this document's connection is not encrypted."}
             else {"This document does not use a TLS connection."});
        return out;
    }
    let Some(info) = view.document_tls().filter(|i| {
        // Same-document fragment navigation must retain the original handshake.
        url::Url::parse(&i.url).ok().is_some_and(|mut original| {
            let mut current = url.clone(); original.set_fragment(None); current.set_fragment(None);
            original == current
        })
    }) else { line(&mut out, "No verified TLS details available for this document."); return out; };
    line(&mut out, "Main document connection (verified by rustls)");
    line(&mut out, if view.load_status() == servo::LoadStatus::Complete { "Page load: complete" }
        else { "Page load: subresources still loading" });
    line(&mut out, &format!("Protocol: {}   Application protocol: {}", info.protocol, if info.alpn.is_empty() { "not negotiated" } else { &info.alpn }));
    line(&mut out, &format!("Cipher: {}", info.cipher));
    line(&mut out, &format!("Server-supplied chain (up to 8): {} certificate(s)", info.certificates.len()));
    line(&mut out, "This describes the document connection, not every subresource."); line(&mut out, "");
    for (index, der) in info.certificates.iter().take(8).enumerate() { certificate(&mut out, der, index); }
    out.truncate(out.len().min(12288)); // ASCII; mirrors the native bounded buffer.
    out
}

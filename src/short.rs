//! Short codes: nine digits to read out over the phone in place of the 52-character code, still
//! with no server of Deskline's own. Each run picks nine random digits. A key made from them
//! (Argon2, so every guess costs real work) signs a record saying "this computer's code is …",
//! put on the public pkarr relay iroh already keeps every endpoint's addresses on (n0's
//! dns.iroh.link). Whoever has the digits makes the same key, fetches the record, checks its
//! signature and its age, and dials the code in it like any other.
//!
//! The digits are the whole secret, so whoever hears them could sign a record too and point
//! them at another computer. That gets nobody in: the person being helped still answers Accept,
//! and the helper sees whose screen comes up. The record is only renewed while the window is
//! open, and one older than `FRESH` counts as no computer at all.
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iroh::address_lookup::pkarr::N0_DNS_PKARR_RELAY_PROD;
use iroh::endpoint_info::{EndpointInfo, UserData};
use iroh::{EndpointId, PublicKey, SecretKey};
use iroh_dns::pkarr::SignedPacket;

/// The record is renewed this often while the window is open,
const RENEW: Duration = Duration::from_secs(4 * 60);
/// and sooner after a try that failed.
const RETRY: Duration = Duration::from_secs(30);
/// An older record is from a window closed a while ago. Also covers clocks minutes apart.
const FRESH: Duration = Duration::from_secs(10 * 60);

/// Nine random digits, as shown: "123 456 789".
pub fn generate() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the system has randomness");
    let digits = format!("{:09}", u64::from_le_bytes(bytes) % 1_000_000_000);
    format!("{} {} {}", &digits[..3], &digits[3..6], &digits[6..])
}

/// The nine digits of a short code typed with or without spaces or dashes.
pub fn parse(s: &str) -> Option<String> {
    let digits: String = s.chars().filter(char::is_ascii_alphanumeric).collect();
    (digits.len() == 9 && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// The key the digits stand for, the same on every computer.
fn key(digits: &str) -> SecretKey {
    let mut seed = [0u8; 32];
    argon2::Argon2::default().hash_password_into(digits.as_bytes(), b"deskline short code 1", &mut seed).expect("fixed sizes argon2 takes");
    SecretKey::from_bytes(&seed)
}

/// Keeps the record for `shown` (a code from `generate`) pointing at the code `target` last
/// sent, renewed while that is `Some` (the window is open). `worked` hears how each try went.
/// Returns once `target`'s sender is gone.
pub fn publish(shown: &str, target: Receiver<Option<EndpointId>>, mut worked: impl FnMut(bool)) {
    let key = key(&shown.replace(' ', ""));
    let mut to = None;
    loop {
        // Nothing to publish: wait for the next target, however long (std's recv_timeout waits
        // like recv when the deadline would overflow).
        let wait = to.map_or(Duration::MAX, |id| {
            let ok = put(&key, id).is_ok();
            worked(ok);
            if ok { RENEW } else { RETRY }
        });
        match target.recv_timeout(wait) {
            Ok(id) => to = id,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn put(key: &SecretKey, id: EndpointId) -> anyhow::Result<()> {
    let record = EndpointInfo::new(key.public()).with_user_data(Some(UserData::try_from(id.to_z32())?));
    let packet = record.to_pkarr_signed_packet(key, FRESH.as_secs() as u32)?;
    agent().put(url(key.public())).send(&packet.to_relay_payload()[..])?;
    Ok(())
}

/// The computer whose window shows `digits` now, or None when no fresh record has them.
/// Blocks (Argon2, then the relay): not for the UI thread.
pub fn find(digits: &str) -> anyhow::Result<Option<EndpointId>> {
    let public = key(digits).public();
    let payload = match agent().get(url(public)).call() {
        Ok(mut response) => response.body_mut().read_to_vec()?,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let packet = SignedPacket::from_relay_payload(&public, &payload)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros() as u64;
    if now.saturating_sub(packet.timestamp().as_micros()) > FRESH.as_micros() as u64 {
        return Ok(None);
    }
    let Some(code) = EndpointInfo::from_pkarr_signed_packet(&packet)?.user_data().map(ToString::to_string) else { return Ok(None) };
    Ok(Some(EndpointId::from_z32(&code)?))
}

fn url(key: PublicKey) -> String {
    format!("{N0_DNS_PKARR_RELAY_PROD}/{}", key.to_z32())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_codes_are_nine_digits_typed_any_way() {
        for _ in 0..100 {
            let shown = generate();
            assert_eq!(shown.len(), 11);
            assert_eq!(parse(&shown), Some(shown.replace(' ', "")));
        }
        assert_eq!(parse("012-345-678").as_deref(), Some("012345678"));
        assert_eq!(parse("12345678"), None);
        assert_eq!(parse("1234567890"), None);
        assert_eq!(parse("12345678a"), None);
        // A long code is never a short one.
        assert_eq!(parse(&crate::net::code(SecretKey::generate().public())), None);
    }

    #[test]
    fn the_same_digits_make_the_same_key() {
        assert_eq!(key("123456789").public(), key("123456789").public());
        assert_ne!(key("123456789").public(), key("123456780").public());
    }

    /// Puts a record on n0's relay and finds it again: needs the internet.
    #[test]
    #[ignore]
    fn short_code_round_trip() {
        let shown = generate();
        let digits = parse(&shown).unwrap();
        let id = SecretKey::generate().public();
        assert_eq!(find(&digits).unwrap(), None);
        put(&key(&digits), id).unwrap();
        assert_eq!(find(&digits).unwrap(), Some(id));
    }
}

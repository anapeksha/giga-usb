//! Wi-Fi provisioning contract. Passwords deliberately implement neither Debug nor Serialize.
use core::cell::RefCell;
use heapless::String;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WifiRequest {
    pub ssid: String<32>,
    pub password: String<64>,
}
impl WifiRequest {
    /// Check byte-level SSID and open/WPA password constraints, not router reachability.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.ssid.is_empty() || self.ssid.as_bytes().contains(&0) {
            return Err("SSID must be 1–32 UTF-8 bytes without NUL");
        }
        let p = self.password.as_bytes();
        if p.is_empty()
            || ((8..=63).contains(&p.len()) && p.iter().all(|b| (32..=126).contains(b)))
            || (p.len() == 64 && p.iter().all(u8::is_ascii_hexdigit))
        {
            Ok(())
        } else {
            Err("password must be empty, 8–63 printable ASCII bytes, or 64 hex digits")
        }
    }
    /// Empty and best-effort overwrite this password buffer, also done on drop.
    /// This does not guarantee zeroization of compiler temporaries or radio-driver copies.
    pub fn clear_password(&mut self) {
        let mut bytes = core::mem::take(&mut self.password).into_bytes();
        bytes.as_mut_slice().fill(0);
        core::hint::black_box(&bytes);
    }
}
impl Drop for WifiRequest {
    fn drop(&mut self) {
        self.clear_password();
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WifiMode {
    Setup,
    Connecting,
    Connected,
}
#[derive(Clone, Serialize)]
pub struct WifiState {
    pub mode: WifiMode,
    pub saved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssid: Option<String<32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String<15>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
}
/// Cooperative-task credential handoff; persistence is owned by the network task.
/// State snapshots never include the password. This is not cross-core synchronization.
pub struct WifiControl(RefCell<(WifiState, Option<WifiRequest>)>);
impl Default for WifiControl {
    fn default() -> Self {
        Self::new()
    }
}
impl WifiControl {
    pub const fn new() -> Self {
        Self(RefCell::new((
            WifiState {
                mode: WifiMode::Setup,
                saved: false,
                ssid: None,
                ip: None,
                error: None,
            },
            None,
        )))
    }
    pub fn set_saved(&self, saved: bool) {
        self.0.borrow_mut().0.saved = saved;
    }
    pub fn state(&self) -> WifiState {
        self.0.borrow().0.clone()
    }
    /// Validate and accept credentials only in setup mode, marking state as connecting.
    /// Success queues a request; it does not mean association or DHCP has completed.
    pub fn submit(&self, request: WifiRequest) -> Result<(), &'static str> {
        request.validate()?;
        let mut s = self.0.borrow_mut();
        if s.0.mode != WifiMode::Setup {
            return Err("Wi-Fi provisioning is only available in setup mode");
        }
        s.0 = WifiState {
            mode: WifiMode::Connecting,
            saved: s.0.saved,
            ssid: Some(request.ssid.clone()),
            ip: None,
            error: None,
        };
        s.1 = Some(request);
        Ok(())
    }
    /// Transfer the pending credentials once to the network task for association.
    pub fn take_request(&self) -> Option<WifiRequest> {
        self.0.borrow_mut().1.take()
    }
    pub fn setup(&self, error: Option<&'static str>) {
        let mut s = self.0.borrow_mut();
        s.0 = WifiState {
            mode: WifiMode::Setup,
            saved: s.0.saved,
            ssid: None,
            ip: Some(String::try_from("192.168.4.1").unwrap()),
            error,
        };
        s.1 = None;
    }
    /// Set a status warning without changing connectivity or stored-record presence.
    pub fn set_error(&self, error: Option<&'static str>) {
        self.0.borrow_mut().0.error = error;
    }
    pub fn connected(&self, ip: String<15>) {
        let mut s = self.0.borrow_mut();
        s.0.mode = WifiMode::Connected;
        s.0.ip = Some(ip);
        s.0.error = None;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request(ssid: &str, password: &str) -> WifiRequest {
        WifiRequest {
            ssid: String::try_from(ssid).unwrap(),
            password: String::try_from(password).unwrap(),
        }
    }
    #[test]
    fn validation() {
        for p in [
            "",
            "12345678",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            assert!(request("网络", p).validate().is_ok());
        }
        for p in [
            "short",
            "1234567é",
            "1234567\n",
            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        ] {
            assert!(request("test", p).validate().is_err());
        }
        assert!(request("", "").validate().is_err());
        assert!(request("a\0b", "").validate().is_err());
        assert!(String::<32>::try_from("网络网络网络网络网络网络").is_err());
        let mut r = request("test", "secret123");
        r.clear_password();
        assert!(r.password.is_empty());
    }
    #[test]
    fn saved_survives_transitions() {
        let c = WifiControl::new();
        assert!(!c.state().saved);
        c.set_saved(true);
        c.setup(None);
        assert!(c.state().saved);
        c.submit(request("saved", "secret123")).unwrap();
        assert!(c.state().saved);
        drop(c.take_request());
        c.connected(String::try_from("10.0.0.2").unwrap());
        assert!(c.state().saved);
        c.setup(Some("connection lost"));
        assert!(c.state().saved);
        c.set_saved(false);
        assert!(!c.state().saved);
    }
    #[test]
    fn persistence_warning_survives_ip_updates_without_changing_saved_presence() {
        for saved in [false, true] {
            let c = WifiControl::new();
            c.set_saved(saved);
            c.submit(request("new network", "secret123")).unwrap();
            drop(c.take_request());
            let warning = if saved {
                "Connected, but new credentials were not saved; previous saved network retained"
            } else {
                "Connected, but credentials were not saved"
            };
            for ip in ["10.0.0.2", "10.0.0.3"] {
                c.connected(String::try_from(ip).unwrap());
                c.set_error(Some(warning));
                let state = c.state();
                assert_eq!(state.mode, WifiMode::Connected);
                assert_eq!(state.ip.as_deref(), Some(ip));
                assert_eq!(state.ssid.as_deref(), Some("new network"));
                assert_eq!(state.error, Some(warning));
                assert_eq!(state.saved, saved);
            }
            c.connected(String::try_from("10.0.0.4").unwrap());
            assert!(c.state().error.is_none());
            assert_eq!(c.state().saved, saved);
        }
    }
    #[test]
    fn transitions() {
        let c = WifiControl::new();
        c.setup(None);
        assert!(c.submit(request("test", "secret123")).is_ok());
        assert_eq!(c.state().mode, WifiMode::Connecting);
        assert!(c.submit(request("other", "")).is_err());
        assert!(c.take_request().is_some());
        assert!(c.take_request().is_none());
        c.connected(String::try_from("10.0.0.2").unwrap());
        assert_eq!(c.state().mode, WifiMode::Connected);
        assert!(c.submit(request("other", "")).is_err());
        c.setup(Some("connection lost"));
        assert_eq!(c.state().mode, WifiMode::Setup);
        assert!(c.state().ssid.is_none());
        assert!(c.submit(request("retry", "")).is_ok());
    }
}

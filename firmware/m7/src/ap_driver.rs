//! AP-only link adapter: cyw43's station link state does not track an open AP.
use core::task::Context;
use embassy_net_driver::{Capabilities, Driver, HardwareAddress, LinkState};

/// Construct only after starting the AP; the AP must remain active for its lifetime.
pub struct ApDriver<D>(pub D);

impl<D: Driver> Driver for ApDriver<D> {
    type RxToken<'a>
        = D::RxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = D::TxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, cx: &mut Context<'_>) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.0.receive(cx)
    }
    fn transmit(&mut self, cx: &mut Context<'_>) -> Option<Self::TxToken<'_>> {
        self.0.transmit(cx)
    }
    fn link_state(&mut self, _cx: &mut Context<'_>) -> LinkState {
        LinkState::Up
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn hardware_address(&self) -> HardwareAddress {
        self.0.hardware_address()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::task::Waker;
    use embassy_net_driver::{RxToken, TxToken};

    struct Token;
    impl RxToken for Token {
        fn consume<R, F>(self, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            f(&mut [])
        }
    }
    impl TxToken for Token {
        fn consume<R, F>(self, len: usize, f: F) -> R
        where
            F: FnOnce(&mut [u8]) -> R,
        {
            let mut bytes = [0; 64];
            f(&mut bytes[..len])
        }
    }
    struct Radio {
        station: LinkState,
        rx_polls: usize,
        tx_polls: usize,
    }
    impl Driver for Radio {
        type RxToken<'a> = Token;
        type TxToken<'a> = Token;
        fn receive(&mut self, _: &mut Context<'_>) -> Option<(Token, Token)> {
            self.rx_polls += 1;
            None
        }
        fn transmit(&mut self, _: &mut Context<'_>) -> Option<Token> {
            self.tx_polls += 1;
            Some(Token)
        }
        fn link_state(&mut self, _: &mut Context<'_>) -> LinkState {
            self.station
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        fn hardware_address(&self) -> HardwareAddress {
            HardwareAddress::Ethernet([2, 1, 2, 3, 4, 5])
        }
    }

    #[test]
    fn ap_stays_up_without_station_and_preserves_io() {
        let mut driver = ApDriver(Radio {
            station: LinkState::Down,
            rx_polls: 0,
            tx_polls: 0,
        });
        let mut cx = Context::from_waker(Waker::noop());
        assert!(driver.link_state(&mut cx) == LinkState::Up);
        assert!(driver.receive(&mut cx).is_none());
        assert!(driver.transmit(&mut cx).is_some());
        assert_eq!(driver.0.rx_polls, 1);
        assert_eq!(driver.0.tx_polls, 1);
        assert_eq!(
            driver.hardware_address(),
            HardwareAddress::Ethernet([2, 1, 2, 3, 4, 5])
        );
        for station in [LinkState::Up, LinkState::Down, LinkState::Down] {
            driver.0.station = station;
            assert!(driver.link_state(&mut cx) == LinkState::Up);
        }
    }
}
